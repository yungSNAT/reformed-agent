//! `llm.oneshot` calls that take a while: they hold up nothing else on their connection, at
//! most 4 run at once across the gateway, and one whose connection closes stops. One test, so
//! no other test in this binary holds a call meanwhile.

#[path = "support/oneshot.rs"]
mod support;

use std::time::{Duration, Instant};

use serde_json::json;
use support::{call, connect, next_json, release_slow, rpc, send, slow_arrived, start_with};

#[tokio::test]
async fn slow_one_shot_calls_are_bounded_and_hold_nothing_up() {
    let (addr, _requests, _home) = start_with(true, Some(Duration::from_secs(1))).await;

    // A turn streams on the connection whilst a one-shot call is held at the model until the
    // turn has ended, so a turn that waited for the call would never end. The connection's
    // write timeout is 1 second, so a connection stalled behind the call would close instead.
    let mut socket = connect(addr).await;
    let created = rpc(&mut socket, 1, "session.create", json!({})).await;
    let session = created["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    send(&mut socket, 2, "llm.oneshot", json!({"input": "SLOW"})).await;
    slow_arrived(1).await;
    send(
        &mut socket,
        3,
        "prompt.submit",
        json!({"session_id": session, "text": "hello"}),
    )
    .await;
    loop {
        let frame = next_json(&mut socket).await;
        assert_ne!(frame["id"], 2, "the held one-shot call answered: {frame}");
        if frame["params"]["type"] == "message.complete" {
            break;
        }
    }
    release_slow();
    loop {
        let frame = next_json(&mut socket).await;
        if frame["id"] == 2 {
            assert!(frame["result"]["text"].is_string(), "{frame}");
            break;
        }
    }

    // 4 slow calls run; a fifth is refused at once.
    let mut busy = connect(addr).await;
    for id in 10..14 {
        send(&mut busy, id, "llm.oneshot", json!({"input": "SLOW"})).await;
    }
    slow_arrived(5).await;
    let mut other = connect(addr).await;
    let reply = call(&mut other, 20, json!({"input": "fast"})).await;
    assert_eq!(reply["error"]["code"], 4090, "{reply}");

    // Closing their connection stops them, and their places free at once.
    drop(busy);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let began = Instant::now();
    let reply = call(&mut other, 21, json!({"input": "fast"})).await;
    assert!(reply["result"]["text"].is_string(), "{reply}");
    assert!(began.elapsed() < Duration::from_secs(1));
}
