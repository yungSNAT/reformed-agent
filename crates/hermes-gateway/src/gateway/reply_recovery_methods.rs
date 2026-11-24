#[derive(Default)]
struct RecoverPassOutcome {
    sent: usize,
    write_failed: bool,
}

impl Gateway {
    /// Keep a record of each turn in flight under `hermes_home`, so a
    /// restart can send what it would otherwise lose (see
    /// [`crate::reply_recovery`]). Off until called; the first call wins.
    /// Call before any adapter routes a turn: it takes over the records
    /// that stopped processes left; `process_alive` tells which writers
    /// still run.
    pub fn enable_reply_recovery(
        &self,
        hermes_home: &std::path::Path,
        process_alive: fn(u32) -> bool,
    ) {
        let dir = crate::reply_recovery::records_dir(hermes_home);
        if self.reply_records.set(dir.clone()).is_ok() {
            let _ = self.reply_writer_alive.set(process_alive);
            let moved = crate::reply_recovery::take_over(&dir, process_alive);
            if moved > 0 {
                tracing::info!(moved, "taking over replies a gateway restart cut off");
            }
        }
    }

    async fn reply_begin(&self, incoming: &IncomingMessage, session_key: &str) -> Option<PathBuf> {
        let dir = self.reply_records.get()?.clone();
        let record = crate::reply_recovery::PendingReply {
            session_key: session_key.to_string(),
            platform: incoming.platform.clone(),
            chat_id: incoming.chat_id.clone(),
            thread_id: Self::reply_thread_id(incoming).map(str::to_string),
            process: crate::reply_recovery::this_process(),
            reply: None,
            attempts: 0,
        };
        tokio::task::spawn_blocking(move || crate::reply_recovery::write(&dir, &record))
            .await
            .ok()
            .flatten()
    }

    async fn reply_begin_send(&self, pending: Option<&PathBuf>, reply: &str) {
        let (Some(dir), Some(path)) = (self.reply_records.get().cloned(), pending.cloned()) else {
            return;
        };
        let reply = reply.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            crate::reply_recovery::begin_send(&dir, &path, &reply)
        })
        .await;
    }

    async fn reply_finish(&self, pending: Option<PathBuf>) {
        let (Some(dir), Some(path)) = (self.reply_records.get().cloned(), pending) else {
            return;
        };
        let _ = tokio::task::spawn_blocking(move || crate::reply_recovery::finish(&dir, &path))
            .await;
    }

    async fn recover_pending_replies_outcome(&self) -> RecoverPassOutcome {
        let Some(dir) = self.reply_records.get().cloned() else {
            return RecoverPassOutcome::default();
        };
        let dir_list = dir.clone();
        let records =
            tokio::task::spawn_blocking(move || crate::reply_recovery::recovering(&dir_list))
                .await
                .unwrap_or_default();
        let mut out = RecoverPassOutcome::default();
        for (path, record) in records {
            let dir_count = dir.clone();
            let path_count = path.clone();
            let mut record_for_attempt = record;
            let Ok((attempt, updated)) = tokio::task::spawn_blocking(move || {
                let attempt = crate::reply_recovery::count_attempt(
                    &dir_count,
                    &path_count,
                    &mut record_for_attempt,
                );
                (attempt, record_for_attempt)
            })
            .await
            else {
                out.write_failed = true;
                continue;
            };
            match attempt {
                crate::reply_recovery::AttemptOutcome::Reserved => {}
                crate::reply_recovery::AttemptOutcome::Exhausted => {
                    warn!(
                        session = %updated.session_key,
                        "dropping a recovered reply after {} recorded attempts",
                        crate::reply_recovery::MAX_SEND_ATTEMPTS
                    );
                    continue;
                }
                crate::reply_recovery::AttemptOutcome::Missing => continue,
                crate::reply_recovery::AttemptOutcome::WriteFailed => {
                    warn!(
                        session = %updated.session_key,
                        "skipping recovered reply: could not persist attempt count"
                    );
                    out.write_failed = true;
                    continue;
                }
            }
            match self
                .send_notify_message_threaded(
                    &updated.platform,
                    &updated.chat_id,
                    &updated.recovery_text(),
                    None,
                    updated.thread_id.as_deref(),
                )
                .await
            {
                Ok(()) => {
                    let dir_done = dir.clone();
                    let path_done = path.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        crate::reply_recovery::mark_delivered(&dir_done, &path_done);
                    })
                    .await;
                    out.sent += 1;
                }
                Err(e) => warn!(
                    session = %updated.session_key,
                    attempt = updated.attempts,
                    "recovered reply not sent: {e}"
                ),
            }
        }
        out
    }

    /// After a restart: send each taken-over reply an earlier process
    /// answered but did not send, and a notice for each turn it cut off.
    /// One pass; returns the number sent. Call once the adapters have
    /// started.
    pub async fn recover_pending_replies(&self) -> usize {
        self.recover_pending_replies_outcome().await.sent
    }

    /// [`Self::recover_pending_replies`], then again every `retry_after`
    /// whilst taken-over records or records held by another running process
    /// remain; each record stops at its own attempt cap. Stops cleanly on
    /// unrecoverable storage errors.
    pub async fn recover_pending_replies_with_retries(
        &self,
        retry_after: std::time::Duration,
    ) -> usize {
        let Some(dir) = self.reply_records.get().cloned() else {
            return 0;
        };
        let alive = self.reply_writer_alive.get().copied();
        let mut sent = 0;
        loop {
            let takeover = if let Some(alive) = alive {
                let dir_take = dir.clone();
                tokio::task::spawn_blocking(move || {
                    crate::reply_recovery::take_over_report(&dir_take, alive)
                })
                .await
                .unwrap_or(crate::reply_recovery::TakeOverReport {
                    moved: 0,
                    unmovable_stopped: true,
                })
            } else {
                crate::reply_recovery::TakeOverReport::default()
            };
            let pass = self.recover_pending_replies_outcome().await;
            sent += pass.sent;
            let io_blocked = pass.write_failed || takeover.unmovable_stopped;
            if io_blocked && pass.sent == 0 && takeover.moved == 0 {
                warn!("stopping reply recovery retries after unrecoverable storage error");
                break;
            }
            let dir_left = dir.clone();
            let left = tokio::task::spawn_blocking(move || {
                crate::reply_recovery::pending_count(&dir_left)
            })
            .await
            .unwrap_or(0);
            if left == 0 {
                break;
            }
            tokio::time::sleep(retry_after).await;
        }
        sent
    }
}
