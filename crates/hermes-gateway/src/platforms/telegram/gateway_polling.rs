use std::time::Duration;

use hermes_core::traits::PlatformAdapter;

use super::{IncomingMessage, PollResult, TelegramAdapter, TelegramTextBatcher};
use crate::gateway::IncomingMessage as GatewayIncomingMessage;
use crate::Gateway;

fn telegram_should_batch_text(msg: &IncomingMessage) -> bool {
    msg.text
        .as_deref()
        .map(|text| !text.trim().is_empty())
        .unwrap_or(false)
        && !msg.is_voice
        && !msg.is_photo
        && !msg.is_sticker
        && !msg.is_document
        && !msg.is_video
        && msg.callback_query_id.is_none()
        && msg.callback_data.is_none()
}

fn telegram_routable_topic_thread(thread_id: Option<i64>) -> Option<i64> {
    match thread_id {
        Some(id) if id > 1 => Some(id),
        _ => None,
    }
}

const TELEGRAM_POLLING_RECONNECT_ERROR_THRESHOLD: u64 = 3;

const TELEGRAM_POLLING_STOPPED_RECHECK_MS: u64 = 500;

fn telegram_polling_should_pause_for_reconnect(consecutive_errors: u64) -> bool {
    consecutive_errors >= TELEGRAM_POLLING_RECONNECT_ERROR_THRESHOLD
}

fn telegram_gateway_message(msg: IncomingMessage) -> GatewayIncomingMessage {
    let text = msg.text.unwrap_or_else(|| {
        if msg.is_voice {
            "[voice message]".to_string()
        } else if msg.is_photo {
            "[photo message]".to_string()
        } else if msg.is_video {
            "[video message]".to_string()
        } else {
            "[unsupported message]".to_string()
        }
    });
    let user_id = msg
        .user_id
        .map(|id| id.to_string())
        .or(msg.username)
        .unwrap_or_else(|| "unknown".to_string());

    let thread_id = telegram_routable_topic_thread(msg.message_thread_id);
    let chat_id = match thread_id {
        Some(thread_id) => format!("{}:{}", msg.chat_id, thread_id),
        _ => msg.chat_id.to_string(),
    };

    GatewayIncomingMessage {
        platform: "telegram".to_string(),
        chat_id,
        user_id,
        text,
        message_id: Some(msg.message_id.to_string()),
        thread_id: thread_id.map(|id| id.to_string()),
        is_dm: msg.chat_id > 0,
    }
}

async fn route_telegram_message(gateway: &Gateway, msg: IncomingMessage) {
    let incoming = telegram_gateway_message(msg);
    if let Err(err) = gateway.route_message(&incoming).await {
        tracing::warn!("Failed to route telegram message: {}", err);
    }
}

async fn route_ready_telegram_messages(gateway: &Gateway, batcher: &mut TelegramTextBatcher) {
    for msg in batcher.drain_ready() {
        route_telegram_message(gateway, msg).await;
    }
}

impl TelegramAdapter {
    /// Routes updates while the Telegram gateway sidecar is running.
    pub async fn poll_gateway(&self, gateway: &Gateway) {
        let batch_delay = Duration::from_millis(self.config().text_batch_delay_ms);
        let mut text_batcher = TelegramTextBatcher::new(batch_delay);
        let mut webhook_cleared = false;

        loop {
            if !self.is_running() {
                webhook_cleared = false;
                tokio::time::sleep(Duration::from_millis(TELEGRAM_POLLING_STOPPED_RECHECK_MS))
                    .await;
                continue;
            }

            if !self.config().polling {
                tokio::time::sleep(Duration::from_millis(TELEGRAM_POLLING_STOPPED_RECHECK_MS))
                    .await;
                continue;
            }

            if !webhook_cleared {
                if let Err(err) = self.delete_webhook(false).await {
                    tracing::warn!("Telegram deleteWebhook before polling failed: {}", err);
                }
                webhook_cleared = true;
            }

            route_ready_telegram_messages(gateway, &mut text_batcher).await;

            match self.poll_with_backoff().await {
                PollResult::Updates(updates) => {
                    for update in updates {
                        if !self.should_process_update(&update) {
                            continue;
                        }
                        let Some(msg) = TelegramAdapter::parse_update(&update) else {
                            continue;
                        };

                        if let (Some(callback_id), Some(callback_data)) =
                            (&msg.callback_query_id, &msg.callback_data)
                        {
                            if callback_data.starts_with("approval:") {
                                if let Err(err) = self
                                    .handle_approval_callback(callback_id, callback_data)
                                    .await
                                {
                                    tracing::warn!(
                                        "Failed to handle telegram approval callback: {}",
                                        err
                                    );
                                }
                                continue;
                            }
                        }

                        if batch_delay.is_zero() || !telegram_should_batch_text(&msg) {
                            route_telegram_message(gateway, msg).await;
                        } else {
                            text_batcher.enqueue(msg);
                        }
                    }
                    if text_batcher.pending_len() > 0 && !batch_delay.is_zero() {
                        tokio::time::sleep(batch_delay).await;
                        route_ready_telegram_messages(gateway, &mut text_batcher).await;
                    }
                }
                PollResult::Backoff { error, delay_ms } => {
                    let consecutive_errors = self.consecutive_error_count();
                    if telegram_polling_should_pause_for_reconnect(consecutive_errors)
                        && self.polling_reconnect_threshold_reached(
                            TELEGRAM_POLLING_RECONNECT_ERROR_THRESHOLD,
                        )
                    {
                        tracing::warn!(
                            consecutive_errors,
                            error = %error,
                            "Telegram polling exceeded reconnect threshold; pausing poll loop until gateway watcher restarts adapter"
                        );
                        self.mark_polling_unhealthy();
                        continue;
                    }

                    tracing::warn!(
                        consecutive_errors,
                        delay_ms,
                        error = %error,
                        "Telegram polling error; backing off"
                    );
                    self.sleep_backoff().await;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "gateway_polling_tests.rs"]
mod tests;
