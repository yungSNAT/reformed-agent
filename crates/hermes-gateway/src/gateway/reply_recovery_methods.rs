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
            let moved = crate::reply_recovery::take_over(&dir, process_alive);
            if moved > 0 {
                tracing::info!(moved, "taking over replies a gateway restart cut off");
            }
        }
    }

    fn reply_begin(&self, incoming: &IncomingMessage, session_key: &str) {
        if let Some(dir) = self.reply_records.get() {
            crate::reply_recovery::write(
                dir,
                &crate::reply_recovery::PendingReply {
                    session_key: session_key.to_string(),
                    platform: incoming.platform.clone(),
                    chat_id: incoming.chat_id.clone(),
                    thread_id: Self::reply_thread_id(incoming).map(str::to_string),
                    process: crate::reply_recovery::this_process(),
                    reply: None,
                    attempts: 0,
                },
            );
        }
    }

    fn reply_answered(&self, session_key: &str, reply: &str) {
        if let Some(dir) = self.reply_records.get() {
            crate::reply_recovery::answered(dir, session_key, reply);
        }
    }

    fn reply_finish(&self, session_key: &str) {
        if let Some(dir) = self.reply_records.get() {
            crate::reply_recovery::finish(dir, session_key);
        }
    }

    /// After a restart: send each taken-over reply an earlier process
    /// answered but did not send, and a notice for each turn it cut off.
    /// One pass; returns the number sent. Call once the adapters have
    /// started.
    pub async fn recover_pending_replies(&self) -> usize {
        let Some(dir) = self.reply_records.get() else {
            return 0;
        };
        let mut sent = 0;
        for (path, mut record) in crate::reply_recovery::recovering(dir) {
            if !crate::reply_recovery::count_attempt(&path, &mut record) {
                warn!(
                    session = %record.session_key,
                    "dropping a recovered reply after {} failed sends",
                    crate::reply_recovery::MAX_SEND_ATTEMPTS
                );
                continue;
            }
            match self
                .send_notify_message_threaded(
                    &record.platform,
                    &record.chat_id,
                    &record.recovery_text(),
                    None,
                    record.thread_id.as_deref(),
                )
                .await
            {
                Ok(()) => {
                    let _ = std::fs::remove_file(&path);
                    sent += 1;
                }
                Err(e) => warn!(
                    session = %record.session_key,
                    attempt = record.attempts,
                    "recovered reply not sent: {e}"
                ),
            }
        }
        sent
    }

    /// [`Self::recover_pending_replies`], then again every `retry_after`
    /// whilst failed sends remain; each record stops at the send cap.
    pub async fn recover_pending_replies_with_retries(
        &self,
        retry_after: std::time::Duration,
    ) -> usize {
        let mut sent = 0;
        for pass in 0..=crate::reply_recovery::MAX_SEND_ATTEMPTS {
            sent += self.recover_pending_replies().await;
            let left = self
                .reply_records
                .get()
                .map(|dir| crate::reply_recovery::recovering(dir).len())
                .unwrap_or(0);
            if left == 0 || pass == crate::reply_recovery::MAX_SEND_ATTEMPTS {
                break;
            }
            tokio::time::sleep(retry_after).await;
        }
        sent
    }
}
