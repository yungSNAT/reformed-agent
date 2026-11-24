use super::*;
use futures::stream::BoxStream;
use futures::StreamExt;
use hermes_core::{AgentError, LlmResponse, StreamChunk};

/// Answers every request with fixed cache usage and records the requests.
struct CacheProvider {
    calls: Mutex<Vec<(Vec<Message>, Option<u32>)>>,
    read: u64,
    write: u64,
}

impl CacheProvider {
    fn new(read: u64, write: u64) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            read,
            write,
        })
    }

    fn calls(&self) -> Vec<(Vec<Message>, Option<u32>)> {
        self.calls.lock().expect("calls").clone()
    }
}

#[async_trait::async_trait]
impl LlmProvider for CacheProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.calls
            .lock()
            .expect("calls")
            .push((messages.to_vec(), max_tokens));
        Ok(LlmResponse {
            message: Message::assistant("."),
            usage: Some(usage(self.read, self.write)),
            model: "claude-test".to_string(),
            finish_reason: Some("max_tokens".to_string()),
        })
    }

    fn chat_completion_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        futures::stream::empty().boxed()
    }
}

fn usage(read: u64, write: u64) -> UsageStats {
    UsageStats {
        prompt_tokens: read + write + 10,
        completion_tokens: 1,
        total_tokens: read + write + 11,
        estimated_cost: None,
        cache_read_tokens: read,
        cache_write_tokens: write,
    }
}

fn request(provider: Arc<CacheProvider>, model: &str, extra_body: Option<Value>) -> CachedRequest {
    CachedRequest {
        provider,
        messages: vec![Message::system("stable"), Message::user("hello")],
        tools: Vec::new(),
        model: model.to_string(),
        extra_body,
        usage: usage(0, 50_000),
    }
}

/// $1 per million prompt tokens, whatever their cache state.
fn plan(max_pings: u32, max_idle_minutes: u64, max_cost_usd: Option<f64>) -> KeepAlivePlan {
    KeepAlivePlan {
        ttl: Duration::from_secs(300),
        max_pings,
        max_idle: Duration::from_secs(max_idle_minutes * 60),
        max_cost_usd,
        cost: Arc::new(|u, _| Some(u.prompt_tokens as f64 / 1_000_000.0)),
        write_price_factor: 1.0,
    }
}

#[tokio::test(start_paused = true)]
async fn pings_at_five_sixths_of_the_ttl_with_a_one_token_cap_until_the_budget_is_spent() {
    let provider = CacheProvider::new(50_000, 0);
    let started = tokio::time::Instant::now();
    let stop = warm(
        "ka-pings",
        request(provider.clone(), "claude-test", None),
        &plan(2, 60, None),
    )
    .await;
    assert_eq!(stop, KeepAliveStop::PingsSpent);
    // 2 pings at 250 s each.
    assert_eq!(started.elapsed(), Duration::from_secs(500));
    let calls = provider.calls();
    assert_eq!(calls.len(), 2);
    for (messages, max_tokens) in &calls {
        assert_eq!(*max_tokens, Some(1));
        assert_eq!(messages.len(), 2, "the last request, replayed as sent");
        assert_eq!(messages[1].content.as_deref(), Some("hello"));
    }
}

#[tokio::test(start_paused = true)]
async fn a_ping_that_misses_ends_warming() {
    let provider = CacheProvider::new(0, 50_000);
    let stop = warm(
        "ka-miss",
        request(provider.clone(), "claude-test", None),
        &plan(5, 60, None),
    )
    .await;
    assert_eq!(stop, KeepAliveStop::Missed);
    assert_eq!(provider.calls().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_idle_cap_stops_pings_that_would_land_after_it() {
    let provider = CacheProvider::new(50_000, 0);
    // 10 minutes: pings at 250 s and 500 s fit, the third (750 s) does not.
    let stop = warm(
        "ka-idle",
        request(provider.clone(), "claude-test", None),
        &plan(12, 10, None),
    )
    .await;
    assert_eq!(stop, KeepAliveStop::IdleCap);
    assert_eq!(provider.calls().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn no_ping_is_sent_whose_worst_case_would_pass_the_cost_cap() {
    let provider = CacheProvider::new(50_000, 0);
    // Worst case per ping 0.05 USD (the 50,000-token prefix written again).
    // Cap 0.12: 2 pings fit, the third would not.
    let stop = warm(
        "ka-cost",
        request(provider.clone(), "claude-test", None),
        &plan(12, 240, Some(0.12)),
    )
    .await;
    assert_eq!(stop, KeepAliveStop::CostCap);
    assert_eq!(provider.calls().len(), 2);

    let unpriced = CacheProvider::new(50_000, 0);
    let mut no_price = plan(3, 60, None);
    no_price.cost = Arc::new(|_, _| None);
    let stop = warm(
        "ka-price",
        request(unpriced.clone(), "claude-test", None),
        &no_price,
    )
    .await;
    assert_eq!(stop, KeepAliveStop::UnknownPrice);
    assert!(unpriced.calls().is_empty());

    // The 1-hour write price: 0.08 per ping, so a 0.12 cap allows 1.
    let hour = CacheProvider::new(50_000, 0);
    let mut one_hour = plan(12, 240, Some(0.12));
    one_hour.write_price_factor = 1.6;
    let stop = warm(
        "ka-hour",
        request(hour.clone(), "claude-test", None),
        &one_hour,
    )
    .await;
    assert_eq!(stop, KeepAliveStop::CostCap);
    assert_eq!(hour.calls().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn arm_sends_nothing_when_off_or_unwarmable_and_a_new_run_cancels_it() {
    let session = "ka-arm";
    for (model, extra, pings) in [
        ("claude-test", None, 0),
        ("gpt-6.2", None, 3),
        (
            "claude-test",
            Some(serde_json::json!({"thinking": {"type": "enabled", "budget_tokens": 2048}})),
            3,
        ),
    ] {
        let provider = CacheProvider::new(50_000, 0);
        remember(session, request(provider.clone(), model, extra));
        arm(session, plan(pings, 60, None));
        tokio::time::sleep(Duration::from_secs(7_200)).await;
        assert!(provider.calls().is_empty(), "{model} pings={pings}");
    }

    let small = CacheProvider::new(50_000, 0);
    let mut tiny = request(small.clone(), "claude-test", None);
    tiny.usage = usage(0, MIN_CACHED_PREFIX_TOKENS - 1);
    remember(session, tiny);
    arm(session, plan(3, 60, None));
    tokio::time::sleep(Duration::from_secs(7_200)).await;
    assert!(
        small.calls().is_empty(),
        "a prefix under the cacheable floor"
    );

    let provider = CacheProvider::new(50_000, 0);
    remember(session, request(provider.clone(), "claude-test", None));
    arm(session, plan(3, 60, None));
    tokio::time::sleep(Duration::from_secs(260)).await;
    assert_eq!(provider.calls().len(), 1, "first ping at 250 s");
    cancel(session);
    tokio::time::sleep(Duration::from_secs(7_200)).await;
    assert_eq!(provider.calls().len(), 1, "a new run cancels the rest");
    forget(session);
}
