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
