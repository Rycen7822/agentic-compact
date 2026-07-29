#![cfg(unix)]

use agentic_compact::error::ErrorCode;
use agentic_compact::protocol::AppEvent;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::{advance, pause, timeout};

mod support;

use support::FakeServer;

fn thread_response(id: &str) -> Value {
    json!({
        "thread": {
            "id": id,
            "status": {"type": "idle"},
            "turns": []
        }
    })
}

#[tokio::test]
async fn initializes_and_forwards_unknown_notifications() {
    let mut server = FakeServer::start().await;
    let client = server.connect().await;
    assert_eq!(client.initialize_result.user_agent, "codex-test/0.1");
    assert_eq!(
        client.initialize_result.codex_home,
        server.codex_home().display().to_string()
    );
    assert_eq!(
        client.endpoint(),
        format!("unix://{}", server.socket_path().display())
    );

    let mut events = client.subscribe();
    server
        .send(json!({"method": "future/notification", "params": {"ignored": true}}))
        .await;
    match timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap()
    {
        AppEvent::UnknownNotification { method } => {
            assert_eq!(method, "future/notification");
        }
        _ => panic!("expected unknown notification"),
    }
    client.close().await;
}

#[tokio::test]
async fn ignores_malformed_json_without_poisoning_the_connection() {
    let mut server = FakeServer::start().await;
    let client = server.connect().await;
    let reading_client = client.clone();
    let reading = tokio::spawn(async move { reading_client.thread_read("thread-a", false).await });

    let request = server.next_request().await;
    server.send_text(r#"{"id":"truncated""#).await;
    tokio::task::yield_now().await;
    assert!(!reading.is_finished());
    server
        .send(json!({
            "id": request["id"],
            "result": thread_response("thread-a")
        }))
        .await;

    assert_eq!(reading.await.unwrap().unwrap().id, "thread-a");
    client.close().await;
}

#[tokio::test]
async fn correlates_out_of_order_responses() {
    let mut server = FakeServer::start().await;
    let client = server.connect().await;
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.thread_read("thread-a", false).await });
    let second_client = client.clone();
    let second = tokio::spawn(async move { second_client.thread_read("thread-b", false).await });

    let request_one = server.next_request().await;
    let request_two = server.next_request().await;
    assert_eq!(request_one["method"], "thread/read");
    assert_eq!(request_two["method"], "thread/read");
    let id_one = request_one["params"]["threadId"].as_str().unwrap();
    let id_two = request_two["params"]["threadId"].as_str().unwrap();

    server
        .send(json!({"id": request_two["id"], "result": thread_response(id_two)}))
        .await;
    server
        .send(json!({"id": request_one["id"], "result": thread_response(id_one)}))
        .await;

    assert_eq!(first.await.unwrap().unwrap().id, "thread-a");
    assert_eq!(second.await.unwrap().unwrap().id, "thread-b");
    client.close().await;
}

#[tokio::test]
async fn retries_overloaded_read_requests() {
    let mut server = FakeServer::start().await;
    let client = server.connect().await;
    let reading_client = client.clone();
    let reading = tokio::spawn(async move { reading_client.thread_read("thread-a", false).await });

    let first = server.next_request().await;
    server
        .send(json!({
            "id": first["id"],
            "error": {"code": -32001, "message": "overloaded"}
        }))
        .await;
    let retry = server.next_request().await;
    assert_eq!(retry["method"], "thread/read");
    assert_eq!(retry["params"], first["params"]);
    assert_ne!(retry["id"], first["id"]);
    server
        .send(json!({
            "id": retry["id"],
            "result": thread_response("thread-a")
        }))
        .await;

    assert_eq!(reading.await.unwrap().unwrap().id, "thread-a");
    client.close().await;
}

#[tokio::test]
async fn does_not_retry_mutating_request_after_timeout() {
    let mut server = FakeServer::start().await;
    let client = server.connect().await;
    let compacting_client = client.clone();
    let compacting = tokio::spawn(async move { compacting_client.compact_start("thread-a").await });

    let request = server.next_request().await;
    assert_eq!(request["method"], "thread/compact/start");
    pause();
    advance(Duration::from_secs(16)).await;
    tokio::task::yield_now().await;

    let error = compacting.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(server.try_next_request().is_none());
    client.close().await;
}

#[tokio::test]
async fn does_not_retry_checkpoint_injection_after_timeout() {
    let mut server = FakeServer::start().await;
    let client = server.connect().await;
    let injecting_client = client.clone();
    let injecting = tokio::spawn(async move {
        injecting_client
            .inject_items("thread-a", vec![json!({"type": "message"})])
            .await
    });

    let request = server.next_request().await;
    assert_eq!(request["method"], "thread/inject_items");
    pause();
    advance(Duration::from_secs(16)).await;
    tokio::task::yield_now().await;

    let error = injecting.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(server.try_next_request().is_none());
    client.close().await;
}

#[tokio::test]
async fn reports_continuation_rejection_without_retry() {
    let mut server = FakeServer::start().await;
    let client = server.connect().await;
    let continuing_client = client.clone();
    let continuing =
        tokio::spawn(async move { continuing_client.start_empty_turn("thread-a").await });

    let request = server.next_request().await;
    assert_eq!(request["method"], "turn/start");
    assert_eq!(request["params"]["input"], json!([]));
    server
        .send(json!({
            "id": request["id"],
            "error": {"code": -32602, "message": "empty input rejected"}
        }))
        .await;

    let error = continuing.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Protocol);
    assert!(server.try_next_request().is_none());
    client.close().await;
}

#[tokio::test]
async fn connection_loss_fails_pending_requests() {
    let mut server = FakeServer::start().await;
    let client = server.connect().await;
    let reading_client = client.clone();
    let reading = tokio::spawn(async move { reading_client.thread_read("thread-a", false).await });
    let request = server.next_request().await;
    assert_eq!(request["method"], "thread/read");
    server.close().await;

    let error = reading.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::SharedAppServerUnavailable);
}
