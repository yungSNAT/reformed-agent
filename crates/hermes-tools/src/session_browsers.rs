//! One browser per gateway chat session.
//!
//! The gateway registers the builtin tools once for every chat. Each turn
//! dispatches the browser tools through [`SessionBrowsers::tool_overrides`]
//! instead, so no session drives or reads another's page, and the registry's
//! own browser tools refuse ([`unbound_browser`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::{stream, StreamExt};

use crate::register_builtins::browser_tool_handlers;
use crate::registry::ToolOverrides;
use crate::tools::browser::{BrowserBackend, BrowserFactory, SharedBrowser};

mod release_queue;
pub use release_queue::{BrowserReleaseQueue, BrowserReleaseSlot, BrowserRetirer};

/// How long a session's browser may go unused before it is released.
pub const SESSION_BROWSER_MAX_IDLE: Duration = Duration::from_secs(300);

const MAX_CONCURRENT_BROWSER_RELEASES: usize = 4;

/// The browser tools, keyed by name, all driving `browser`.
pub fn browser_overrides(browser: &SharedBrowser) -> ToolOverrides {
    browser_tool_handlers(browser.clone())
        .into_iter()
        .map(|(handler, _)| (handler.schema().name, handler))
        .collect()
}

/// A browser that refuses every call, for the tools of a registry that
/// several chat sessions share.
pub fn unbound_browser() -> SharedBrowser {
    SharedBrowser::lazy(|| {
        crate::backends::browser::unavailable_browser_backend(
            "Browser tools need a chat session of their own here, and this call has none",
        )
    })
}

/// Each gateway chat session's browser, made on its first browser call and
/// closed when the session ends.
#[derive(Clone)]
pub struct SessionBrowsers {
    factory: Arc<BrowserFactory>,
    sessions: Arc<Mutex<HashMap<String, (SharedBrowser, ToolOverrides)>>>,
}

/// Browser handlers pinned to a turn's session generation.
#[derive(Clone)]
pub struct BrowserLease(ToolOverrides);

impl BrowserLease {
    pub fn tool_overrides(&self) -> ToolOverrides {
        self.0.clone()
    }
}

impl std::fmt::Debug for BrowserLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserLease")
            .finish_non_exhaustive()
    }
}

impl SessionBrowsers {
    /// Sessions whose backends `factory` builds.
    pub fn new(factory: impl Fn() -> Arc<dyn BrowserBackend> + Send + Sync + 'static) -> Self {
        Self {
            factory: Arc::new(factory),
            sessions: Arc::default(),
        }
    }

    /// The browser tools for a turn of the session `session_key`.
    pub fn tool_overrides(&self, session_key: &str) -> ToolOverrides {
        self.sessions()
            .entry(session_key.to_string())
            .or_insert_with(|| {
                let factory = self.factory.clone();
                let browser = SharedBrowser::lazy(move || factory());
                let overrides = browser_overrides(&browser);
                (browser, overrides)
            })
            .1
            .clone()
    }

    /// Close the browser of the session `session_key`, which has ended.
    /// Its handlers, including any a running turn holds, fail from now on.
    pub async fn close(&self, session_key: &str) {
        if let Some(browser) = self.retire(session_key) {
            browser.release_closed_backend().await;
        }
    }

    fn retire(&self, session_key: &str) -> Option<SharedBrowser> {
        self.sessions().remove(session_key).map(|(browser, _)| {
            browser.mark_closed();
            browser
        })
    }

    fn try_retire_without_backend(&self, session_key: &str) -> bool {
        let mut sessions = self.sessions();
        if sessions
            .get(session_key)
            .is_some_and(|(browser, _)| !browser.try_close_without_backend())
        {
            return false;
        }
        sessions.remove(session_key);
        true
    }

    /// Release each browser no call has used within `max_idle`. A session
    /// that calls again gets a new one.
    pub async fn release_idle(&self, max_idle: Duration) {
        let browsers: Vec<SharedBrowser> = self
            .sessions()
            .values()
            .map(|(browser, _)| browser.clone())
            .collect();
        stream::iter(browsers)
            .for_each_concurrent(MAX_CONCURRENT_BROWSER_RELEASES, |browser| async move {
                browser.release_if_idle(max_idle).await;
            })
            .await;
    }

    /// Close every session's browser, at shutdown.
    pub async fn close_all(&self) {
        let removed: Vec<_> = self
            .sessions()
            .drain()
            .map(|(_, (browser, _))| {
                browser.mark_closed();
                browser
            })
            .collect();
        stream::iter(removed)
            .for_each_concurrent(MAX_CONCURRENT_BROWSER_RELEASES, |browser| async move {
                browser.release_closed_backend().await;
            })
            .await;
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<String, (SharedBrowser, ToolOverrides)>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::browser::tests::{
        drain_commands, remote_browser, session_creations, spawn_cdp_websocket, EnvScope,
    };
    use crate::register_builtins::tests::{MockSkillProvider, MockTerminalBackend};
    use crate::ToolRegistry;
    use crate::{isolated_browser_backend_from_env, register_builtin_tools_with_browser};
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc::UnboundedReceiver;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    /// Creates `vendor_session_<n>` for the nth request, on browser
    /// `browsers[(n - 1) % browsers.len()]`.
    struct NumberedSessions {
        browsers: Vec<String>,
        created: AtomicUsize,
    }

    impl Respond for NumberedSessions {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let index = self.created.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({
                "id": format!("vendor_session_{}", index + 1),
                "connectUrl": self.browsers[index % self.browsers.len()],
            }))
        }
    }

    /// The gateway's registry and session browsers on a Browserbase stub,
    /// whose first session runs on browser `a` and second on browser `b`.
    /// Fields drop in order, so backends release their sessions whilst the
    /// stub still runs.
    struct Gateway {
        registry: ToolRegistry,
        browsers: SessionBrowsers,
        api: MockServer,
        a: UnboundedReceiver<(usize, Value)>,
        b: UnboundedReceiver<(usize, Value)>,
        _home: tempfile::TempDir,
        _scope: EnvScope,
    }

    async fn gateway() -> Gateway {
        let scope = EnvScope::new();
        let home = tempfile::tempdir().expect("temp hermes home");
        let (a_addr, a) = spawn_cdp_websocket(remote_browser(|_| json!({"nodes": []}))).await;
        let (b_addr, b) = spawn_cdp_websocket(remote_browser(|_| json!({"nodes": []}))).await;
        let api = MockServer::builder().start().await;
        Mock::given(method("POST"))
            .and(path("/v1/sessions"))
            .respond_with(NumberedSessions {
                browsers: [a_addr, b_addr]
                    .map(|addr| format!("ws://{addr}/devtools/browser/B"))
                    .to_vec(),
                created: AtomicUsize::new(0),
            })
            .mount(&api)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/v1/sessions/vendor_session_\d+$"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&api)
            .await;
        std::env::set_var("HERMES_HOME", home.path());
        std::env::set_var("HERMES_BROWSER_BACKEND", "browserbase");
        std::env::set_var("BROWSERBASE_API_KEY", "bb-key");
        std::env::set_var("BROWSERBASE_PROJECT_ID", "proj");
        std::env::set_var("BROWSERBASE_BASE_URL", api.uri());
        let registry = ToolRegistry::new();
        register_builtin_tools_with_browser(
            &registry,
            Arc::new(MockTerminalBackend),
            Arc::new(MockSkillProvider),
            unbound_browser(),
        );
        Gateway {
            registry,
            browsers: SessionBrowsers::new(isolated_browser_backend_from_env),
            api,
            a,
            b,
            _home: home,
            _scope: scope,
        }
    }

    impl Gateway {
        /// `browser_snapshot` in a turn of the session `session_key`, as the
        /// gateway binds it.
        fn snapshot(&self, session_key: &str) -> String {
            let overrides = self.browsers.tool_overrides(session_key);
            self.registry
                .dispatch_with("browser_snapshot", json!({}), &overrides)
        }

        async fn created(&self) -> usize {
            session_creations(&self.api, "/v1/sessions").await
        }

        async fn released(&self, session: usize) -> usize {
            session_creations(&self.api, &format!("/v1/sessions/vendor_session_{session}")).await
        }
    }

    /// How many snapshots reached the browser recording `commands`.
    fn snapshots(commands: &mut UnboundedReceiver<(usize, Value)>) -> usize {
        drain_commands(commands)
            .iter()
            .filter(|command| command["method"] == "Accessibility.getFullAXTree")
            .count()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_session_drives_a_vendor_session_and_page_of_its_own() {
        let mut gateway = gateway().await;

        for chat in ["telegram:alice", "telegram:alice", "telegram:bob"] {
            let result = gateway.snapshot(chat);
            assert!(result.contains("snapshot"), "{chat}: {result}");
        }

        assert_eq!(
            gateway.created().await,
            2,
            "the sessions shared a vendor session"
        );
        assert_eq!(snapshots(&mut gateway.a), 2, "a call missed alice's page");
        assert_eq!(
            snapshots(&mut gateway.b),
            1,
            "bob's call reached alice's page"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_ended_session_releases_its_vendor_session_once_and_refuses_its_old_handlers() {
        let gateway = gateway().await;
        gateway.snapshot("telegram:alice");
        gateway.snapshot("telegram:bob");
        let alice_turn = gateway.browsers.tool_overrides("telegram:alice");

        gateway.browsers.close("telegram:alice").await;

        assert_eq!(
            gateway.released(1).await,
            1,
            "alice's session was not released"
        );
        assert_eq!(
            gateway.released(2).await,
            0,
            "bob's session ended with alice's"
        );
        let stale = gateway
            .registry
            .dispatch_with("browser_snapshot", json!({}), &alice_turn);
        assert!(stale.contains("browser was closed"), "{stale}");
        assert_eq!(
            gateway.created().await,
            2,
            "a closed session's handler started a session"
        );

        gateway.browsers.close_all().await;
        assert_eq!(
            gateway.released(2).await,
            1,
            "shutdown left bob's session open"
        );
        drop(alice_turn);
        assert_eq!(
            gateway.released(1).await,
            1,
            "alice's session was released twice"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deferred_release_keeps_the_worker_owned_until_the_vendor_replies() {
        let gateway = gateway().await;
        let requested = Arc::new(tokio::sync::Notify::new());
        let notify = requested.clone();
        let (reply, wait_for_reply) = std::sync::mpsc::channel();
        let wait_for_reply = Mutex::new(wait_for_reply);
        Mock::given(method("POST"))
            .and(path("/v1/sessions/vendor_session_1"))
            .respond_with(move |_: &Request| {
                notify.notify_one();
                let replied = wait_for_reply
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .is_ok();
                ResponseTemplate::new(if replied { 200 } else { 500 })
            })
            .with_priority(1)
            .mount(&gateway.api)
            .await;
        let queue = BrowserReleaseQueue::start(gateway.browsers.clone());
        let old = gateway.browsers.tool_overrides("telegram:alice");
        assert!(gateway.snapshot("telegram:alice").contains("snapshot"));
        assert!(gateway.snapshot("telegram:bob").contains("snapshot"));
        queue.retirer().retire("telegram:alice").await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), requested.notified())
            .await
            .expect("the release worker did not reach the vendor");

        let mut drain = Box::pin(queue.drain());
        assert!(futures::poll!(drain.as_mut()).is_pending());
        let response = gateway
            .registry
            .dispatch_with("browser_snapshot", json!({}), &old);
        assert!(response.contains("browser was closed"), "{response}");
        assert!(gateway.snapshot("telegram:bob").contains("snapshot"));
        reply.send(()).unwrap();
        drain.await.unwrap();
        assert_eq!(gateway.released(1).await, 1);
        assert_eq!(gateway.released(2).await, 0);
        gateway.browsers.close_all().await;
        assert_eq!(gateway.released(2).await, 1);
    }

    /// Releases run together, so a vendor that answers slowly holds no other
    /// session's release back.
    #[tokio::test(flavor = "multi_thread")]
    async fn sessions_are_released_together_not_one_after_another() {
        let gateway = gateway().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/v1/sessions/vendor_session_\d+$"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(1)))
            .with_priority(1)
            .mount(&gateway.api)
            .await;
        let chats = ["telegram:alice", "telegram:bob", "telegram:carol"];

        for chat in chats {
            gateway.snapshot(chat);
        }
        let started = std::time::Instant::now();
        gateway.browsers.release_idle(Duration::ZERO).await;
        let idle = started.elapsed();
        assert!(
            idle < Duration::from_secs(2),
            "idle releases ran one after another: {idle:?}"
        );

        for chat in chats {
            gateway.snapshot(chat);
        }
        let started = std::time::Instant::now();
        gateway.browsers.close_all().await;
        let closing = started.elapsed();
        assert!(
            closing < Duration::from_secs(2),
            "closing ran one after another: {closing:?}"
        );
        for session in 1..=6 {
            assert_eq!(gateway.released(session).await, 1, "session {session}");
        }
    }

    #[tokio::test]
    async fn shutdown_retires_all_handlers_before_releasing_in_batches() {
        use futures::FutureExt;

        let sessions = SessionBrowsers::new(|| {
            crate::backends::browser::unavailable_browser_backend("fixture")
        });
        for index in 0..5 {
            sessions.tool_overrides(&index.to_string());
        }
        let browsers: Vec<_> = sessions
            .sessions()
            .values()
            .map(|(browser, _)| browser.clone())
            .collect();
        let mut active = Vec::new();
        for browser in &browsers {
            active.push(browser.lock().await.unwrap());
        }

        let mut shutdown = std::pin::pin!(sessions.close_all());
        assert!(futures::poll!(shutdown.as_mut()).is_pending());
        // Every browser refuses new work whilst the active calls hold the locks.
        for browser in &browsers {
            match browser.lock().now_or_never() {
                Some(Err(error)) => assert!(error.to_string().contains("browser was closed")),
                Some(Ok(_)) => panic!("a queued browser was still open"),
                None => panic!("a retired browser waited for the backend lock"),
            }
        }

        // The first release batch has queued for its locks. Dropping the active
        // calls wakes those releases and leaves the next batch's lock available.
        drop(active);
        let releasing = browsers
            .iter()
            .filter(|browser| !browser.backend_lock_available())
            .count();
        assert_eq!(
            releasing, 4,
            "unexpected number of concurrent browser releases"
        );
        shutdown.await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_idle_session_releases_its_vendor_session_and_a_later_call_starts_another() {
        let gateway = gateway().await;
        gateway.snapshot("telegram:alice");

        gateway
            .browsers
            .release_idle(SESSION_BROWSER_MAX_IDLE)
            .await;
        assert_eq!(
            gateway.released(1).await,
            0,
            "released before the idle period"
        );

        gateway.browsers.release_idle(Duration::ZERO).await;
        assert_eq!(
            gateway.released(1).await,
            1,
            "the idle session was not released"
        );

        let result = gateway.snapshot("telegram:alice");
        assert!(result.contains("snapshot"), "{result}");
        assert_eq!(
            gateway.created().await,
            2,
            "the later call started no session"
        );
        assert_eq!(
            gateway.released(1).await,
            1,
            "the idle session was released twice"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_gateway_registry_refuses_browser_tools_without_a_session() {
        let gateway = gateway().await;

        let result = gateway.registry.dispatch("browser_snapshot", json!({}));

        assert!(result.contains("need a chat session"), "{result}");
        assert_eq!(
            gateway.created().await,
            0,
            "an unbound call started a session"
        );
    }
}
