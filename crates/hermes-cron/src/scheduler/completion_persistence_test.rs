struct GatedCompletionPersistence {
    file: FileJobPersistence,
    saving: Notify,
    release_save: Notify,
}

#[async_trait::async_trait]
impl JobPersistence for GatedCompletionPersistence {
    async fn save_jobs(
        &self,
        jobs: &[CronJob],
    ) -> Result<(), crate::persistence::CronPersistenceError> {
        self.file.save_jobs(jobs).await
    }

    async fn load_jobs(&self) -> Result<Vec<CronJob>, crate::persistence::CronPersistenceError> {
        self.file.load_jobs().await
    }

    async fn save_job(
        &self,
        job: &CronJob,
    ) -> Result<(), crate::persistence::CronPersistenceError> {
        if job.last_run.is_some() {
            self.saving.notify_one();
            self.release_save.notified().await;
        }
        self.file.save_job(job).await
    }

    async fn delete_job(&self, id: &str) -> Result<(), crate::persistence::CronPersistenceError> {
        self.file.delete_job(id).await
    }
}

#[tokio::test]
async fn manual_completion_finishes_saving_before_job_removal() {
    use std::future::Future;
    use std::task::{Context, Waker};

    let dir = tempdir().expect("tempdir");
    let persistence = Arc::new(GatedCompletionPersistence {
        file: FileJobPersistence::with_dir(dir.path().to_path_buf()),
        saving: Notify::new(),
        release_save: Notify::new(),
    });
    let executor = Arc::new(BlockingTestExecutor::default());
    executor.release.add_permits(1);
    let scheduler = Arc::new(CronScheduler::new_with_executor(
        persistence.clone(),
        executor,
    ));
    let id = scheduler
        .create_job(due_test_job("manual completion and removal"))
        .await
        .expect("create job");

    let completion = {
        let scheduler = scheduler.clone();
        let id = id.clone();
        tokio::spawn(async move { scheduler.run_job(&id).await })
    };
    tokio::time::timeout(StdDuration::from_secs(2), persistence.saving.notified())
        .await
        .expect("completion reached persistence");

    assert!(
        scheduler.jobs.try_lock().is_err(),
        "the job map must stay locked until the completion save finishes"
    );
    let mut remove = std::pin::pin!(scheduler.remove_job(&id));
    assert!(
        remove
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "removal must wait for the completion save"
    );
    persistence.release_save.notify_one();
    completion
        .await
        .expect("join completion")
        .expect("complete job");
    remove.await.expect("remove job");
    assert!(scheduler.get_job(&id).await.is_none());
    assert!(persistence.load_jobs().await.expect("read jobs").is_empty());
}

#[derive(Default)]
struct RecoveringTestExecutor {
    attempts: AtomicUsize,
    deliveries: StdMutex<Vec<(String, crate::job::DeliverConfig)>>,
}

#[async_trait::async_trait]
impl CronJobExecutor for RecoveringTestExecutor {
    async fn run_job(&self, _job: &CronJob) -> Result<AgentResult, CronError> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if attempt < 2 {
            return Err(CronError::Scheduler("fixture execution failed".into()));
        }
        Ok(AgentResult {
            messages: vec![
                Message::assistant("Earlier answer"),
                Message::assistant("é".repeat(32_001)),
                Message::user("Fixture note"),
            ],
            finished_naturally: true,
            total_turns: 2,
            ..AgentResult::default()
        })
    }

    async fn deliver_error(
        &self,
        error: &str,
        target: &crate::job::DeliverConfig,
    ) -> Result<(), CronError> {
        self.deliveries
            .lock()
            .unwrap()
            .push((error.to_owned(), target.clone()));
        Err(CronError::Scheduler("fixture delivery failed".into()))
    }
}

#[tokio::test]
async fn cron_completion_reports_failures_and_persists_manual_recovery() {
    let dir = tempdir().unwrap();
    let persistence = Arc::new(FileJobPersistence::with_dir(dir.path().to_path_buf()));
    let executor = Arc::new(RecoveringTestExecutor::default());
    let mut scheduler = CronScheduler::new_with_executor(persistence.clone(), executor.clone());
    let (sender, mut events) = broadcast::channel(3);
    scheduler.set_completion_broadcast(sender);
    let mut job = due_test_job("Read the fixture");
    let prior_runs = 2;
    job.run_count = prior_runs;
    let delivery = crate::job::DeliverConfig {
        target: crate::job::DeliverTarget::Local,
        platform: None,
    };
    job.deliver = Some(delivery.clone());
    let id = scheduler.create_job(job).await.unwrap();

    let error = scheduler.run_job(&id).await.unwrap_err();
    let execution_error = "Scheduler error: fixture execution failed";
    assert_eq!(error.to_string(), execution_error);
    let manual_error = events.try_recv().unwrap();
    assert_eq!(manual_error.trigger, "manual");
    assert_eq!(manual_error.job_id, id);
    assert!(!manual_error.ok);
    assert_eq!(manual_error.error.as_deref(), Some(execution_error));
    assert_eq!(
        scheduler.get_job(&id).await.unwrap().status,
        JobStatus::Active
    );

    assert_eq!(scheduler.tick_due_jobs().await, 1);
    let scheduled_error = tokio::time::timeout(StdDuration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scheduled_error.trigger, "schedule");
    assert_eq!(scheduled_error.job_id, id);
    assert!(!scheduled_error.ok);
    assert_eq!(scheduled_error.error.as_deref(), Some(execution_error));
    tokio::time::timeout(StdDuration::from_secs(5), async {
        while scheduler.running_job_ids.lock().await.contains(&id) {
            sleep(StdDuration::from_millis(5)).await;
        }
    })
    .await
    .expect("scheduled failure finishes and clears the running marker");
    assert_eq!(
        scheduler.get_job(&id).await.unwrap().status,
        JobStatus::Failed
    );
    assert_eq!(
        persistence.load_jobs().await.unwrap()[0].status,
        JobStatus::Failed
    );

    let result = scheduler.run_job(&id).await.unwrap();
    assert_eq!(result.total_turns, 2);
    let recovered = events.try_recv().unwrap();
    assert_eq!(recovered.trigger, "manual");
    assert_eq!(recovered.job_id, id);
    assert!(recovered.ok);
    assert_eq!(recovered.total_turns, Some(2));
    assert_eq!(
        recovered.assistant_snippet.unwrap(),
        "é".repeat(2_000) + "…"
    );
    let job = scheduler.get_job(&id).await.unwrap();
    assert_eq!(job.status, JobStatus::Active);
    assert_eq!(job.run_count, prior_runs);
    assert!(job.last_run.is_some());
    assert_eq!(
        job.last_output.as_deref(),
        Some(("é".repeat(32_000) + "…").as_str())
    );
    assert_eq!(persistence.load_jobs().await.unwrap(), [job]);
    assert_eq!(
        *executor.deliveries.lock().unwrap(),
        vec![(execution_error.to_owned(), delivery); 2]
    );
}
