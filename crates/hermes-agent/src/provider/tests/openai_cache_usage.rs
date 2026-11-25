fn openai_reply_with_usage(usage: Value) -> Value {
    serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
        "model": "m",
        "usage": usage,
    })
}

#[test]
fn openai_cached_prompt_tokens_count_as_cache_reads() {
    let reply = openai_reply_with_usage(serde_json::json!({
        "prompt_tokens": 1000,
        "completion_tokens": 10,
        "total_tokens": 1010,
        "prompt_tokens_details": {"cached_tokens": 896}
    }));
    let usage = parse_openai_response(&reply).unwrap().usage.unwrap();
    assert_eq!(usage.prompt_tokens, 1000);
    assert_eq!(usage.cache_read_tokens, 896);
    assert_eq!(usage.cache_write_tokens, 0);
}

#[test]
fn deepseek_prompt_cache_hits_count_as_cache_reads() {
    let reply = openai_reply_with_usage(serde_json::json!({
        "prompt_tokens": 1000,
        "completion_tokens": 10,
        "total_tokens": 1010,
        "prompt_cache_hit_tokens": 960,
        "prompt_cache_miss_tokens": 40
    }));
    let usage = parse_openai_response(&reply).unwrap().usage.unwrap();
    assert_eq!(usage.cache_read_tokens, 960);
}

#[test]
fn the_streamed_usage_chunk_carries_cache_reads() {
    let chunk = serde_json::json!({
        "choices": [],
        "usage": {
            "prompt_tokens": 500,
            "completion_tokens": 5,
            "total_tokens": 505,
            "prompt_tokens_details": {"cached_tokens": 384}
        }
    });
    let usage = parse_sse_chunk(&chunk).unwrap().usage.unwrap();
    assert_eq!(usage.cache_read_tokens, 384);
}

#[test]
fn cache_reads_never_exceed_the_prompt_and_default_to_none() {
    let inflated = openai_reply_with_usage(serde_json::json!({
        "prompt_tokens": 100,
        "completion_tokens": 1,
        "total_tokens": 101,
        "prompt_tokens_details": {"cached_tokens": 5000}
    }));
    let usage = parse_openai_response(&inflated).unwrap().usage.unwrap();
    assert_eq!(usage.cache_read_tokens, 100);

    let plain = openai_reply_with_usage(serde_json::json!({
        "prompt_tokens": 100,
        "completion_tokens": 1,
        "total_tokens": 101
    }));
    let usage = parse_openai_response(&plain).unwrap().usage.unwrap();
    assert_eq!(usage.cache_read_tokens, 0);
}

#[test]
fn openrouter_cache_writes_are_counted_beside_reads() {
    let reply = openai_reply_with_usage(serde_json::json!({
        "prompt_tokens": 194,
        "completion_tokens": 2,
        "total_tokens": 196,
        "prompt_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 100}
    }));
    let usage = parse_openai_response(&reply).unwrap().usage.unwrap();
    assert_eq!(usage.cache_read_tokens, 0);
    assert_eq!(usage.cache_write_tokens, 100);

    let chunk = serde_json::json!({
        "choices": [],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 1,
            "total_tokens": 101,
            "prompt_tokens_details": {"cached_tokens": 80, "cache_write_tokens": 500}
        }
    });
    let usage = parse_sse_chunk(&chunk).unwrap().usage.unwrap();
    assert_eq!((usage.cache_read_tokens, usage.cache_write_tokens), (80, 20));
}
