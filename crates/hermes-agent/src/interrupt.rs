//! Interrupt mechanism for the agent loop.
//!
//! Provides a thread-safe interrupt controller that allows external threads
//! (TUI input handler, message receiver, Ctrl+C signal handler) to request
//! the agent to stop its current tool-calling loop and optionally redirect
//! to a new message.
//!
//! Corresponds to Python `run_agent.py`'s `interrupt()` / `clear_interrupt()`
//! / `_interrupt_requested` system.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hermes_core::AgentError;

/// Thread-safe interrupt controller for the agent loop.
///
/// Clone-friendly: all clones share the same underlying state via `Arc`.
///
/// # Usage
///
/// ```rust,ignore
/// let ctrl = InterruptController::new();
///
/// // From another thread (e.g. Ctrl+C handler):
/// ctrl.interrupt(Some("New user message".into()));
///
/// // In the agent loop, before each LLM call / tool execution:
/// ctrl.check_interrupt()?;
/// ```
#[derive(Debug, Clone)]
pub struct InterruptController {
    /// Atomic flag — set to `true` when an interrupt is requested.
    flag: Arc<AtomicBool>,
    /// Optional redirect message that triggered the interrupt.
    redirect_message: Arc<Mutex<Option<String>>>,
    /// Wakes [`Self::interrupted`] waiters when an interrupt is requested.
    notify: Arc<tokio::sync::Notify>,
    /// Steer text the model has not seen yet, several steers joined by
    /// newlines.
    pending_steer: Arc<Mutex<Option<String>>>,
}

impl InterruptController {
    /// Create a new interrupt controller in the non-interrupted state.
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            redirect_message: Arc::new(Mutex::new(None)),
            notify: Arc::new(tokio::sync::Notify::new()),
            pending_steer: Arc::new(Mutex::new(None)),
        }
    }

    /// Queue guidance for the model without stopping the turn. The agent
    /// loop delivers it with the turn's latest tool result before its next
    /// model call. One still pending when a run ends stays here for
    /// [`Self::take_steer`]; the ACP server and the gateway make a controller
    /// per turn, so today they drop it. Returns `false` for blank text.
    pub fn steer(&self, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        if let Ok(mut guard) = self.pending_steer.lock() {
            *guard = Some(match guard.take() {
                Some(earlier) => format!("{earlier}\n{text}"),
                None => text.to_string(),
            });
        }
        true
    }

    /// Take the steer text the model has not seen yet.
    pub fn take_steer(&self) -> Option<String> {
        self.pending_steer
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
    }

    /// Put back steer text taken but not delivered, ahead of any sent since.
    pub(crate) fn restore_steer(&self, text: String) {
        if let Ok(mut guard) = self.pending_steer.lock() {
            *guard = Some(match guard.take() {
                Some(later) => format!("{text}\n{later}"),
                None => text,
            });
        }
    }

    /// Request the agent to interrupt its current loop.
    ///
    /// Optionally provide a redirect message (e.g. new user input that
    /// should be processed instead of continuing the current tool chain).
    ///
    /// A pending steer is dropped: it was meant for a turn that now stops.
    pub fn interrupt(&self, message: Option<String>) {
        if let Ok(mut guard) = self.redirect_message.lock() {
            *guard = message;
        }
        self.take_steer();
        // Set the flag AFTER storing the message so readers always see
        // a consistent (flag=true, message=Some) pair.
        self.flag.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    /// Resolves once an interrupt is pending, so a wait (such as for the
    /// next chunk of a stream) can end on it instead of polling the flag.
    pub async fn interrupted(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // Registered before the check, so an interrupt between the two
            // still wakes this waiter.
            notified.as_mut().enable();
            if self.is_interrupted() {
                return;
            }
            notified.await;
        }
    }

    /// Clear any pending interrupt request.
    pub fn clear_interrupt(&self) {
        self.flag.store(false, Ordering::Release);
        if let Ok(mut guard) = self.redirect_message.lock() {
            *guard = None;
        }
    }

    /// Check whether an interrupt has been requested.
    pub fn is_interrupted(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// If an interrupt is pending, consume it (clear flag + redirect) and return `Some(redirect)`.
    ///
    /// Used by the agent loop for **graceful** shutdown with
    /// [`hermes_core::AgentResult::interrupted`] instead of
    /// [`AgentError::Interrupted`]. Call sites that still need the legacy error
    /// should use [`Self::check_interrupt`].
    pub fn take_interrupt_graceful(&self) -> Option<Option<String>> {
        if !self.is_interrupted() {
            return None;
        }
        let msg = self.take_redirect_message();
        self.clear_interrupt();
        Some(msg)
    }

    /// Take the redirect message (if any), leaving `None` in its place.
    pub fn take_redirect_message(&self) -> Option<String> {
        if let Ok(mut guard) = self.redirect_message.lock() {
            guard.take()
        } else {
            None
        }
    }

    /// Check the interrupt flag and return `Err(AgentError::Interrupted)`
    /// if set. This is the primary check point used inside the agent loop
    /// before each LLM call and tool execution.
    pub fn check_interrupt(&self) -> Result<(), AgentError> {
        if self.is_interrupted() {
            let message = self.take_redirect_message();
            Err(AgentError::Interrupted { message })
        } else {
            Ok(())
        }
    }
}

impl Default for InterruptController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_controller_is_not_interrupted() {
        let ctrl = InterruptController::new();
        assert!(!ctrl.is_interrupted());
        assert!(ctrl.check_interrupt().is_ok());
    }

    #[test]
    fn test_interrupt_sets_flag() {
        let ctrl = InterruptController::new();
        ctrl.interrupt(None);
        assert!(ctrl.is_interrupted());
    }

    #[test]
    fn test_interrupt_with_message() {
        let ctrl = InterruptController::new();
        ctrl.interrupt(Some("new input".into()));
        assert!(ctrl.is_interrupted());

        match ctrl.check_interrupt() {
            Err(AgentError::Interrupted { message }) => {
                assert_eq!(message.as_deref(), Some("new input"));
            }
            other => panic!("expected Interrupted, got {:?}", other),
        }
    }

    #[test]
    fn steers_queue_in_order_without_interrupting() {
        let ctrl = InterruptController::new();
        assert!(!ctrl.steer("  "));
        assert!(ctrl.steer(" first "));
        assert!(ctrl.steer("second"));
        assert!(!ctrl.is_interrupted());
        let taken = ctrl.take_steer().expect("pending steer");
        assert_eq!(taken, "first\nsecond");
        ctrl.steer("third");
        ctrl.restore_steer(taken);
        assert_eq!(ctrl.take_steer().as_deref(), Some("first\nsecond\nthird"));
        assert_eq!(ctrl.take_steer(), None);
    }

    #[test]
    fn an_interrupt_drops_a_pending_steer() {
        let ctrl = InterruptController::new();
        ctrl.steer("too late");
        ctrl.interrupt(None);
        assert_eq!(ctrl.take_steer(), None);
    }

    #[test]
    fn test_clear_interrupt() {
        let ctrl = InterruptController::new();
        ctrl.interrupt(Some("msg".into()));
        assert!(ctrl.is_interrupted());

        ctrl.clear_interrupt();
        assert!(!ctrl.is_interrupted());
        assert!(ctrl.check_interrupt().is_ok());
    }

    #[test]
    fn test_take_redirect_message_consumes() {
        let ctrl = InterruptController::new();
        ctrl.interrupt(Some("hello".into()));

        let msg = ctrl.take_redirect_message();
        assert_eq!(msg.as_deref(), Some("hello"));

        // Second take returns None
        let msg2 = ctrl.take_redirect_message();
        assert!(msg2.is_none());
    }

    #[tokio::test]
    async fn interrupted_wakes_a_waiter() {
        let ctrl = InterruptController::new();
        let waiter = tokio::spawn({
            let ctrl = ctrl.clone();
            async move { ctrl.interrupted().await }
        });
        tokio::task::yield_now().await;
        ctrl.interrupt(None);
        tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("the waiter woke")
            .expect("waiter task");
        // Already pending: resolves at once.
        tokio::time::timeout(std::time::Duration::from_secs(1), ctrl.interrupted())
            .await
            .expect("pending interrupt");
    }

    #[test]
    fn test_clone_shares_state() {
        let ctrl1 = InterruptController::new();
        let ctrl2 = ctrl1.clone();

        ctrl1.interrupt(Some("shared".into()));
        assert!(ctrl2.is_interrupted());
    }
}
