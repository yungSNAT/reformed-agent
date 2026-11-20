use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinError, JoinHandle};

use super::{BrowserLease, SessionBrowsers, SharedBrowser};

const QUEUED_BROWSER_RELEASES: usize = 16;

/// Owns and joins the worker that releases retired gateway browsers.
pub struct BrowserReleaseQueue {
    retirer: BrowserRetirer,
    stop: oneshot::Sender<()>,
    worker: JoinHandle<()>,
}

/// Retires the current browser and hands it to the release worker.
#[derive(Clone)]
pub struct BrowserRetirer {
    browsers: SessionBrowsers,
    send: mpsc::Sender<SharedBrowser>,
}

/// A queue slot that can accept one retired browser synchronously.
pub struct BrowserReleaseSlot<'a> {
    browsers: &'a SessionBrowsers,
    permit: mpsc::Permit<'a, SharedBrowser>,
}

impl BrowserReleaseSlot<'_> {
    pub fn retire(self, session_key: &str) {
        if let Some(browser) = self.browsers.retire(session_key) {
            self.permit.send(browser);
        }
    }
}

impl BrowserReleaseQueue {
    pub fn start(browsers: SessionBrowsers) -> Self {
        Self::with_capacity(browsers, QUEUED_BROWSER_RELEASES)
    }

    fn with_capacity(browsers: SessionBrowsers, capacity: usize) -> Self {
        let (send, mut receive) = mpsc::channel::<SharedBrowser>(capacity);
        let (stop, mut stopping) = oneshot::channel();
        let worker = tokio::spawn(async move {
            loop {
                let browser = tokio::select! {
                    biased;
                    _ = &mut stopping => break,
                    browser = receive.recv() => browser,
                };
                let Some(browser) = browser else { return };
                browser.release_closed_backend().await;
            }
            receive.close();
            while let Some(browser) = receive.recv().await {
                browser.release_closed_backend().await;
            }
        });
        Self {
            retirer: BrowserRetirer { browsers, send },
            stop,
            worker,
        }
    }

    pub fn retirer(&self) -> BrowserRetirer {
        self.retirer.clone()
    }

    /// Wait for every queued release attempt, including failed vendor releases.
    /// Call once gateway ingress and the other retirement producers have stopped.
    pub async fn drain(self) -> Result<(), JoinError> {
        let _ = self.stop.send(());
        self.worker.await
    }
}

impl BrowserRetirer {
    pub fn ensure_running(&self) -> Result<(), &'static str> {
        if self.send.is_closed() {
            return Err("Browser release worker has stopped");
        }
        Ok(())
    }

    pub fn lease(&self, session_key: &str) -> BrowserLease {
        BrowserLease(self.browsers.tool_overrides(session_key))
    }

    /// Retire an absent or dormant browser without reserving room in the queue.
    /// An active backend or a browser with a held lock still needs the worker.
    pub fn try_retire_without_backend(&self, session_key: &str) -> bool {
        self.browsers.try_retire_without_backend(session_key)
    }

    pub fn try_reserve(&self) -> Result<Option<BrowserReleaseSlot<'_>>, &'static str> {
        let permit = match self.send.try_reserve() {
            Ok(permit) => permit,
            Err(mpsc::error::TrySendError::Full(())) => return Ok(None),
            Err(mpsc::error::TrySendError::Closed(())) => {
                return Err("Browser release worker has stopped")
            }
        };
        Ok(Some(BrowserReleaseSlot {
            browsers: &self.browsers,
            permit,
        }))
    }

    pub async fn reserve(&self) -> Result<BrowserReleaseSlot<'_>, &'static str> {
        let permit = self
            .send
            .reserve()
            .await
            .map_err(|_| "Browser release worker has stopped")?;
        Ok(BrowserReleaseSlot {
            browsers: &self.browsers,
            permit,
        })
    }

    /// Capacity is reserved before the browser is taken from the active map.
    /// Cancelling during backpressure leaves the active browser in place.
    pub async fn retire(&self, session_key: &str) -> Result<(), &'static str> {
        self.reserve().await?.retire(session_key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::task::Poll;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn cancellation_at_capacity_keeps_the_active_browser_and_drain_waits_for_releases() {
        let browsers = SessionBrowsers::new(|| {
            crate::backends::browser::unavailable_browser_backend("browser fixture")
        });
        let queue = BrowserReleaseQueue::with_capacity(browsers.clone(), 1);
        let retirer = queue.retirer();
        for key in ["busy", "queued", "active"] {
            browsers.tool_overrides(key);
        }
        let busy = browsers.sessions()["busy"].0.clone();
        let queued = browsers.sessions()["queued"].0.clone();
        let active = browsers.sessions()["active"].0.clone();
        let operation = busy.lock().await.unwrap();
        retirer.retire("busy").await.unwrap();
        // The worker takes the first browser and waits for its running call.
        let slot = tokio::time::timeout(Duration::from_secs(5), retirer.send.reserve())
            .await
            .expect("worker did not take the retired browser")
            .unwrap();
        drop(slot);
        retirer.retire("queued").await.unwrap();
        {
            let mut blocked = Box::pin(retirer.retire("active"));
            assert!(matches!(futures::poll!(blocked.as_mut()), Poll::Pending));
        }
        assert!(active.lock().await.is_ok());
        assert!(queued.lock().await.is_err());
        assert!(browsers.sessions().contains_key("active"));

        let mut drain = Box::pin(queue.drain());
        assert!(matches!(futures::poll!(drain.as_mut()), Poll::Pending));
        drop(operation);
        tokio::time::timeout(Duration::from_secs(5), drain)
            .await
            .expect("retired browsers were not drained")
            .unwrap();
        assert!(busy.lock().await.is_err());
        assert!(retirer.retire("active").await.is_err());
        assert!(active.lock().await.is_ok());
        browsers.close_all().await;
        assert!(active.lock().await.is_err());
    }
}
