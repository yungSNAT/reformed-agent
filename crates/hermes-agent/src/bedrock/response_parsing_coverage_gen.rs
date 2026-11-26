mod bedrock {
    mod response_parsing {
        mod coverage_gen {
            use super::super::super::super::*;

            fn llm_msg(err: AgentError) -> String {
                match err {
                    AgentError::LlmApi(message) => message,
                    _ => panic!("unexpected error variant"),
                }
            }

            fn expect_err<T>(result: Result<T, AgentError>) -> AgentError {
                match result {
                    Ok(_) => panic!("expected an error"),
                    Err(err) => err,
                }
            }

            fn expect_ok<T>(result: Result<T, AgentError>) -> T {
                match result {
                    Ok(value) => value,
                    Err(_) => panic!("expected success"),
                }
            }

            fn str_header(name: &str, value: &str) -> Vec<u8> {
                let mut out = vec![name.len() as u8];
                out.extend_from_slice(name.as_bytes());
                out.push(7);
                out.extend_from_slice(&(value.len() as u16).to_be_bytes());
                out.extend_from_slice(value.as_bytes());
                out
            }

            fn build_frame(headers: &[u8], payload: &[u8]) -> Vec<u8> {
                let total = 16 + headers.len() + payload.len();
                let mut frame = Vec::new();
                frame.extend_from_slice(&(total as u32).to_be_bytes());
                frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
                let prelude_crc = crc32_ieee(&frame);
                frame.extend_from_slice(&prelude_crc.to_be_bytes());
                frame.extend_from_slice(headers);
                frame.extend_from_slice(payload);
                let message_crc = crc32_ieee(&frame);
                frame.extend_from_slice(&message_crc.to_be_bytes());
                frame
            }

            #[test]
            fn crc32_matches_known_vector() {
                assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926);
            }

            #[test]
            fn parse_bedrock_response_uses_openai_like_shape() {
                let json = serde_json::json!({
                    "model": "remote-model",
                    "choices": [{
                        "message": {"content": "hello", "reasoning": "thinking"},
                        "finish_reason": "stop"
                    }]
                });
                let resp = expect_ok(parse_bedrock_response(&json, "fallback"));
                assert_eq!(resp.message.content.as_deref(), Some("hello"));
                assert_eq!(resp.model, "remote-model");
                assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
                assert_eq!(resp.message.reasoning_content.as_deref(), Some("thinking"));
            }

            #[test]
            fn parse_bedrock_response_rejects_invalid_shape() {
                let json = serde_json::json!({"error": "boom"});
                let msg = llm_msg(expect_err(parse_bedrock_response(&json, "m")));
                assert!(msg.contains("Invalid Bedrock response shape"));
            }

            #[test]
            fn parse_bedrock_response_skips_tool_use_without_name() {
                let json = serde_json::json!({
                    "output": {"message": {"content": [
                        {"text": "hi"},
                        {"toolUse": {"toolUseId": "t1", "name": "", "input": {"a": 1}}}
                    ]}},
                    "stopReason": "max_tokens"
                });
                let resp = expect_ok(parse_bedrock_response(&json, "m"));
                assert_eq!(resp.message.content.as_deref(), Some("hi"));
                assert!(resp.message.tool_calls.is_none());
                assert_eq!(resp.finish_reason.as_deref(), Some("length"));
            }

            #[test]
            fn parse_openai_like_response_full_and_fallbacks() {
                let json = serde_json::json!({
                    "model": "m2",
                    "choices": [{
                        "message": {
                            "content": "text",
                            "tool_calls": [
                                {"id": "c1", "function": {"name": "f", "arguments": "{\"a\":1}"}},
                                {"id": "c2", "function": {}},
                                {"id": "c3"},
                                {"function": {"name": "g"}}
                            ],
                            "reasoning_content": "rc"
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
                });
                let resp = parse_openai_like_response(&json, "fb").expect("parsed");
                let calls = resp.message.tool_calls.expect("tool calls");
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].id, "c1");
                assert_eq!(calls[0].function.name, "f");
                assert_eq!(calls[0].function.arguments, "{\"a\":1}");
                assert_eq!(calls[1].id, "");
                assert_eq!(calls[1].function.name, "g");
                assert_eq!(calls[1].function.arguments, "{}");
                let usage = resp.usage.expect("usage");
                assert_eq!(usage.prompt_tokens, 3);
                assert_eq!(usage.completion_tokens, 4);
                assert_eq!(usage.total_tokens, 7);
                assert_eq!(resp.model, "m2");
                assert_eq!(resp.message.reasoning_content.as_deref(), Some("rc"));
                assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
            }

            #[test]
            fn parse_openai_like_response_empty_tools_and_missing_fields() {
                let json = serde_json::json!({
                    "choices": [{"message": {"tool_calls": []}}]
                });
                let resp = parse_openai_like_response(&json, "fb").expect("parsed");
                assert_eq!(resp.model, "fb");
                assert_eq!(resp.message.content.as_deref(), Some(""));
                assert!(resp.message.tool_calls.is_none());
                assert!(resp.usage.is_none());
                assert!(resp.finish_reason.is_none());
                assert!(resp.message.reasoning_content.is_none());

                assert!(
                    parse_openai_like_response(&serde_json::json!({"choices": []}), "fb").is_none()
                );
                assert!(
                    parse_openai_like_response(&serde_json::json!({"choices": [{}]}), "fb")
                        .is_none()
                );
            }

            #[test]
            fn stream_events_invalid_shape_errors() {
                let json = serde_json::json!({"nope": 1});
                let msg = llm_msg(expect_err(parse_bedrock_stream_events(&json, "m")));
                assert!(msg.contains("Invalid Bedrock ConverseStream shape"));
            }

            #[test]
            fn stream_events_tool_use_delta_with_object_input() {
                let json = serde_json::json!([
                    {"contentBlockDelta": {"contentBlockIndex": 0, "delta": {"toolUse": {
                        "toolUseId": "id-1", "name": "search", "input": {"x": 1}
                    }}}},
                    {"contentBlockDelta": {"contentBlockIndex": 1, "delta": {"toolUse": {
                        "toolUseId": "id-2", "name": "   ", "input": "{}"
                    }}}},
                    {"messageStop": {"stopReason": "end_turn"}}
                ]);
                let resp = expect_ok(parse_bedrock_stream_events(&json, "m"));
                let calls = resp.message.tool_calls.expect("tool calls");
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "id-1");
                assert_eq!(calls[0].function.name, "search");
                assert_eq!(calls[0].function.arguments, "{\"x\":1}");
                assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
            }

            #[test]
            fn stream_events_text_and_reasoning() {
                let json = serde_json::json!([
                    {"contentBlockDelta": {"delta": {"text": "he"}}},
                    {"contentBlockDelta": {"delta": {"text": "llo"}}},
                    {"contentBlockDelta": {"delta": {"reasoningContent": {"text": "why"}}}},
                    {"messageStop": {"stopReason": "end_turn"}}
                ]);
                let resp = expect_ok(parse_bedrock_stream_events(&json, "m"));
                assert_eq!(resp.message.content.as_deref(), Some("hello"));
                assert_eq!(resp.message.reasoning_content.as_deref(), Some("why"));
                assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
            }

            #[test]
            fn stream_events_without_tools_maps_stop_reason() {
                let json = serde_json::json!({"stream": [
                    {"messageStop": {"stopReason": "max_tokens"}}
                ]});
                let resp = expect_ok(parse_bedrock_stream_events(&json, "model-x"));
                assert!(resp.message.content.is_none());
                assert!(resp.message.tool_calls.is_none());
                assert!(resp.message.reasoning_content.is_none());
                assert_eq!(resp.finish_reason.as_deref(), Some("length"));
                assert_eq!(resp.model, "model-x");
            }

            #[test]
            fn normalize_empty_input_is_empty_object() {
                assert_eq!(normalize_tool_input_arguments("   "), "{}");
            }

            #[test]
            fn take_frame_rejects_short_total_length() {
                let mut buf = vec![0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0];
                let msg = llm_msg(expect_err(take_aws_event_stream_message(&mut buf)));
                assert!(msg.contains("frame length: 8"));
            }

            #[test]
            fn take_frame_rejects_large_headers_length() {
                let mut buf = vec![0u8; 16];
                buf[3] = 16;
                buf[7] = 1;
                let msg = llm_msg(expect_err(take_aws_event_stream_message(&mut buf)));
                assert!(msg.contains("headers length: 1"));
            }

            #[test]
            fn take_frame_rejects_bad_prelude_checksum() {
                let mut buf = vec![0u8; 16];
                buf[3] = 16;
                let msg = llm_msg(expect_err(take_aws_event_stream_message(&mut buf)));
                assert!(msg.contains("prelude checksum"));
            }

            #[test]
            fn take_frame_rejects_bad_message_checksum() {
                let mut frame = build_frame(&[], b"{}");
                let last = frame.len() - 1;
                frame[last] ^= 0xff;
                let msg = llm_msg(expect_err(take_aws_event_stream_message(&mut frame)));
                assert!(msg.contains("message checksum"));
            }

            #[test]
            fn take_and_decode_valid_frame() {
                let headers = str_header(":event-type", "messageStop");
                let mut buf = build_frame(&headers, br#"{"stopReason":"end_turn"}"#);
                buf.push(0xAA);
                let message = match expect_ok(take_aws_event_stream_message(&mut buf)) {
                    Some(m) => m,
                    None => panic!("expected a message"),
                };
                assert_eq!(buf, vec![0xAA]);
                assert_eq!(
                    message.headers.get(":event-type").map(String::as_str),
                    Some("messageStop")
                );
                let value =
                    expect_ok(decode_bedrock_event_stream_message(&message)).expect("value");
                assert_eq!(
                    value,
                    serde_json::json!({"messageStop": {"stopReason": "end_turn"}})
                );
                let chunks = expect_ok(bedrock_stream_event_to_chunks(&value));
                assert_eq!(chunks.len(), 1);
                assert_eq!(chunks[0].finish_reason.as_deref(), Some("stop"));
            }

            #[test]
            fn decode_empty_payload_and_bad_json() {
                let empty = AwsEventStreamMessage {
                    headers: std::collections::HashMap::new(),
                    payload: Vec::new(),
                };
                assert!(expect_ok(decode_bedrock_event_stream_message(&empty)).is_none());

                let bad = AwsEventStreamMessage {
                    headers: std::collections::HashMap::new(),
                    payload: b"not json".to_vec(),
                };
                let msg = llm_msg(expect_err(decode_bedrock_event_stream_message(&bad)));
                assert!(msg.contains("JSON parse failed"));
            }

            #[test]
            fn decode_passthrough_and_wrapping() {
                let direct = AwsEventStreamMessage {
                    headers: std::collections::HashMap::new(),
                    payload: br#"{"contentBlockDelta":{"delta":{"text":"a"}}}"#.to_vec(),
                };
                let value = expect_ok(decode_bedrock_event_stream_message(&direct)).expect("value");
                assert_eq!(
                    value,
                    serde_json::json!({"contentBlockDelta": {"delta": {"text": "a"}}})
                );

                let mut headers = std::collections::HashMap::new();
                headers.insert(":event-type".to_string(), "contentBlockDelta".to_string());
                let wrapped = AwsEventStreamMessage {
                    headers,
                    payload: br#"{"contentBlockIndex":0,"delta":{"text":"hi"}}"#.to_vec(),
                };
                let value =
                    expect_ok(decode_bedrock_event_stream_message(&wrapped)).expect("value");
                let chunks = expect_ok(bedrock_stream_event_to_chunks(&value));
                assert_eq!(chunks.len(), 1);
                let delta = chunks[0].delta.as_ref().expect("delta");
                assert_eq!(delta.content.as_deref(), Some("hi"));

                let mut headers = std::collections::HashMap::new();
                headers.insert(":event-type".to_string(), String::new());
                let plain = AwsEventStreamMessage {
                    headers,
                    payload: br#"{"foo":1}"#.to_vec(),
                };
                let value = expect_ok(decode_bedrock_event_stream_message(&plain)).expect("value");
                assert_eq!(value, serde_json::json!({"foo": 1}));
            }

            #[test]
            fn decode_exception_message_type_errors() {
                let mut headers = std::collections::HashMap::new();
                headers.insert(":message-type".to_string(), "exception".to_string());
                let msg = AwsEventStreamMessage {
                    headers,
                    payload: br#"{"message":"bad"}"#.to_vec(),
                };
                assert!(decode_bedrock_event_stream_message(&msg).is_err());
            }

            #[test]
            fn headers_parse_all_value_types() {
                let mut raw = Vec::new();
                raw.extend_from_slice(&[1, b'a', 0]);
                raw.extend_from_slice(&[1, b'b', 1]);
                raw.extend_from_slice(&[1, b'c', 2, 0xff]);
                raw.extend_from_slice(&[1, b'd', 3, 0x01, 0x00]);
                raw.extend_from_slice(&[1, b'e', 4, 0, 0, 0, 5]);
                raw.extend_from_slice(&[1, b'f', 5, 0, 0, 0, 0, 0, 0, 1, 0]);
                raw.extend_from_slice(&[
                    1, b'g', 8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                ]);
                raw.extend_from_slice(&[1, b'h', 6, 0, 2, 0xab, 0xcd]);
                raw.extend_from_slice(&[1, b'i', 7, 0, 3, b'x', b'y', b'z']);
                raw.extend_from_slice(&[1, b'j', 9]);
                raw.extend(0u8..16);
                let headers = expect_ok(parse_aws_event_stream_headers(&raw));
                let get = |k: &str| headers.get(k).cloned().unwrap_or_default();
                assert_eq!(get("a"), "true");
                assert_eq!(get("b"), "false");
                assert_eq!(get("c"), "-1");
                assert_eq!(get("d"), "256");
                assert_eq!(get("e"), "5");
                assert_eq!(get("f"), "256");
                assert_eq!(get("g"), "-1");
                assert_eq!(get("h"), "abcd");
                assert_eq!(get("i"), "xyz");
                assert_eq!(get("j"), "000102030405060708090a0b0c0d0e0f");
                assert_eq!(headers.len(), 10);
            }

            #[test]
            fn headers_parse_errors() {
                let msg = llm_msg(expect_err(parse_aws_event_stream_headers(&[0])));
                assert!(msg.contains("header name"));

                let msg = llm_msg(expect_err(parse_aws_event_stream_headers(&[5, b'a'])));
                assert!(msg.contains("header name"));

                let msg = llm_msg(expect_err(parse_aws_event_stream_headers(&[1, 0xff, 0])));
                assert!(msg.contains("header name UTF-8"));

                let msg = llm_msg(expect_err(parse_aws_event_stream_headers(&[1, b'a'])));
                assert!(msg.contains("header value"));

                let msg = llm_msg(expect_err(parse_aws_event_stream_headers(&[1, b'a', 4, 0])));
                assert!(msg.contains("header value"));

                let msg = llm_msg(expect_err(parse_aws_event_stream_headers(&[
                    1, b'a', 7, 0, 1, 0xff,
                ])));
                assert!(msg.contains("header UTF-8"));

                let msg = llm_msg(expect_err(parse_aws_event_stream_headers(&[1, b'a', 42])));
                assert!(msg.contains("Unsupported Bedrock event stream header value type: 42"));
            }
        }
    }
}
