use std::convert::Infallible;
use std::fs;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::body::Bytes;
use axum::body::to_bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::Request;
use axum::http::Response;
use axum::routing::post;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::*;

#[derive(Debug)]
struct CapturedRequest {
    headers: HeaderMap,
    body: Vec<u8>,
}

const MIDSTREAM_USAGE: &str = concat!(
    "event: codex.rate_limits\n",
    "data: {\"type\":\"codex.rate_limits\",\"rate_limits\":{\"primary\":{\"used_percent\":37.5,\"window_minutes\":300,\"reset_at\":1704068000}}}\n\n"
);
const HEADER_MATCHING_USAGE: &str = concat!(
    "event: codex.rate_limits\n",
    "data: {\"type\":\"codex.rate_limits\",\"rate_limits\":{\"primary\":{\"used_percent\":42.0,\"window_minutes\":300,\"reset_at\":1704069000}}}\n\n"
);
const COMPLETED_REPLY: &str = concat!(
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"response-1\"}}\n\n"
);

#[tokio::test]
async fn forwarding_replaces_account_headers_without_changing_body() {
    let captured = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/responses", post(capture_request))
        .with_state(Arc::clone(&captured));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test upstream");
    let upstream_addr = listener.local_addr().expect("read test upstream address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve test upstream");
    });

    let state = ProxyState {
        client: reqwest::Client::new(),
        upstream_base: Url::parse(&format!("http://{upstream_addr}"))
            .expect("parse test upstream URL"),
        paying_account: PayingAccount {
            label: "Pool B".to_string(),
            access_token: "secondary-token".to_string(),
            account_id: "secondary-account".to_string(),
            usage: Arc::new(Mutex::new(None)),
            usage_store: None,
        },
        message_boundary: Arc::new(MessageBoundaryDetector::default()),
    };
    let body = br#"{"input":[{"role":"user","content":"keep this exactly"}]}"#;
    let request = Request::builder()
        .method("POST")
        .uri("/responses")
        .header("authorization", "Bearer main-token")
        .header("chatgpt-account-id", "main-account")
        .header("x-keep-me", "unchanged")
        .body(Body::from(body.as_slice()))
        .expect("build test request");

    let response = forward_request(&state, request)
        .await
        .expect("forward request");
    assert_eq!(response.status(), StatusCode::OK);

    let captured = captured
        .lock()
        .expect("lock captured request")
        .take()
        .expect("upstream received request");
    assert_eq!(
        (
            captured.headers.get("authorization"),
            captured.headers.get("chatgpt-account-id"),
            captured.headers.get("x-keep-me"),
            captured.body,
        ),
        (
            Some(&HeaderValue::from_static("Bearer secondary-token")),
            Some(&HeaderValue::from_static("secondary-account")),
            Some(&HeaderValue::from_static("unchanged")),
            body.to_vec(),
        )
    );
}

#[tokio::test]
async fn forwarding_records_reply_usage_for_the_paying_account() {
    let temp = tempfile::tempdir().expect("create temporary usage directory");
    let usage_path = temp.path().join("pool.usage.json");
    let app = Router::new().route("/responses", post(reply_with_usage));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test upstream");
    let upstream_addr = listener.local_addr().expect("read test upstream address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve test upstream");
    });

    let mut state = test_state(upstream_addr);
    state.paying_account.usage_store = Some(Arc::new(UsageStore::load(&usage_path, 0).store));
    let request = Request::builder()
        .method("POST")
        .uri("/responses")
        .body(Body::empty())
        .expect("build test request");

    forward_request(&state, request)
        .await
        .expect("forward request");

    assert_eq!(
        (
            state.paying_account.label.as_str(),
            *state
                .paying_account
                .usage
                .lock()
                .expect("lock paying account usage"),
        ),
        (
            "Pool B",
            Some(AccountUsage {
                used_percent: 12.5,
                window_minutes: 300,
                resets_at: 1_704_069_000,
            }),
        )
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(usage_path).expect("read saved account usage"))
            .expect("parse saved account usage");
    assert_eq!(
        persisted,
        json!({
            "accounts": {
                "Pool B": {
                    "used_percent": 12.5,
                    "window_minutes": 300,
                    "resets_at": 1_704_069_000,
                }
            }
        })
    );
}

#[tokio::test]
async fn reply_without_usage_preserves_paying_account_usage() {
    let app = Router::new().route("/responses", post(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test upstream");
    let upstream_addr = listener.local_addr().expect("read test upstream address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve test upstream");
    });

    let state = test_state(upstream_addr);
    let existing = AccountUsage {
        used_percent: 44.0,
        window_minutes: 60,
        resets_at: 1_800_000_000,
    };
    *state
        .paying_account
        .usage
        .lock()
        .expect("lock paying account usage") = Some(existing);
    let request = Request::builder()
        .method("POST")
        .uri("/responses")
        .body(Body::empty())
        .expect("build test request");

    forward_request(&state, request)
        .await
        .expect("forward request");

    assert_eq!(
        *state
            .paying_account
            .usage
            .lock()
            .expect("lock paying account usage"),
        Some(existing)
    );
}

#[tokio::test]
async fn forwarding_reads_midstream_usage_without_delaying_or_altering_reply() {
    let (release_matching_usage, wait_for_matching_usage) = oneshot::channel();
    let (release_completion, wait_for_completion) = oneshot::channel();
    let wait_for_matching_usage = Arc::new(Mutex::new(Some(wait_for_matching_usage)));
    let wait_for_completion = Arc::new(Mutex::new(Some(wait_for_completion)));
    let app = Router::new().route(
        "/responses",
        post({
            let wait_for_matching_usage = Arc::clone(&wait_for_matching_usage);
            let wait_for_completion = Arc::clone(&wait_for_completion);
            move || {
                let wait_for_matching_usage = wait_for_matching_usage
                    .lock()
                    .expect("lock first stream gate")
                    .take()
                    .expect("take first stream gate");
                let wait_for_completion = wait_for_completion
                    .lock()
                    .expect("lock second stream gate")
                    .take()
                    .expect("take second stream gate");
                async move { streaming_usage_reply(wait_for_matching_usage, wait_for_completion) }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test upstream");
    let upstream_addr = listener.local_addr().expect("read test upstream address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve test upstream");
    });

    let state = test_state(upstream_addr);
    let request = Request::builder()
        .method("POST")
        .uri("/responses")
        .body(Body::empty())
        .expect("build test request");
    let response = forward_request(&state, request)
        .await
        .expect("forward request");
    let mut body = response.into_body().into_data_stream();
    let mut received = read_next_sse_event(&mut body).await;

    assert_eq!(
        *state
            .paying_account
            .usage
            .lock()
            .expect("lock paying account usage"),
        Some(AccountUsage {
            used_percent: 37.5,
            window_minutes: 300,
            resets_at: 1_704_068_000,
        })
    );

    release_matching_usage
        .send(())
        .expect("release matching usage event");
    received.extend(read_next_sse_event(&mut body).await);
    assert_eq!(
        *state
            .paying_account
            .usage
            .lock()
            .expect("lock paying account usage"),
        Some(AccountUsage {
            used_percent: 42.0,
            window_minutes: 300,
            resets_at: 1_704_069_000,
        })
    );

    release_completion
        .send(())
        .expect("release completion event");
    while let Some(chunk) = timeout(Duration::from_secs(5), body.next())
        .await
        .expect("forwarded stream made progress")
    {
        received.extend(chunk.expect("read forwarded stream chunk"));
    }
    assert_eq!(
        received,
        format!("{MIDSTREAM_USAGE}{HEADER_MATCHING_USAGE}{COMPLETED_REPLY}").into_bytes()
    );
}

#[test]
fn message_with_several_tool_steps_keeps_one_boundary_for_normal_and_streaming_requests() {
    let detector = MessageBoundaryDetector::default();
    let requests = [
        json!({
            "input": [{
                "type": "message",
                "role": "user",
                "content": "start",
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-1"}
            }],
            "stream": false
        }),
        json!({
            "input": [{
                "type": "function_call_output",
                "call_id": "call-1",
                "output": "first",
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-1"}
            }],
            "stream": false
        }),
        json!({
            "input": [{
                "type": "custom_tool_call_output",
                "call_id": "call-2",
                "output": "second",
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-1"}
            }],
            "stream": true
        }),
        json!({
            "input": [{
                "type": "tool_search_output",
                "call_id": "call-3",
                "status": "completed",
                "tools": [],
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-1"}
            }],
            "stream": true
        }),
        json!({
            "input": [{
                "type": "message",
                "role": "user",
                "content": "next",
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-2"}
            }],
            "stream": true
        }),
    ];

    let actual = requests
        .iter()
        .map(|request| {
            detector.classify(&serde_json::to_vec(request).expect("serialize request fixture"))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        actual,
        vec![
            Some(MessageRequestKind::NewMessage),
            Some(MessageRequestKind::FollowUp),
            Some(MessageRequestKind::FollowUp),
            Some(MessageRequestKind::FollowUp),
            Some(MessageRequestKind::NewMessage),
        ]
    );
}

fn streaming_usage_reply(
    wait_for_matching_usage: oneshot::Receiver<()>,
    wait_for_completion: oneshot::Receiver<()>,
) -> Response<Body> {
    let first = futures::stream::once(async {
        Ok::<_, Infallible>(Bytes::from_static(MIDSTREAM_USAGE.as_bytes()))
    });
    let second = futures::stream::once(async move {
        wait_for_matching_usage
            .await
            .expect("wait to send matching usage event");
        Ok::<_, Infallible>(Bytes::from_static(HEADER_MATCHING_USAGE.as_bytes()))
    });
    let third = futures::stream::once(async move {
        wait_for_completion
            .await
            .expect("wait to send completion event");
        Ok::<_, Infallible>(Bytes::from_static(COMPLETED_REPLY.as_bytes()))
    });
    let mut response = Response::new(Body::from_stream(first.chain(second).chain(third)));
    response.headers_mut().insert(
        "x-codex-primary-used-percent",
        HeaderValue::from_static("42.0"),
    );
    response.headers_mut().insert(
        "x-codex-primary-window-minutes",
        HeaderValue::from_static("300"),
    );
    response.headers_mut().insert(
        "x-codex-primary-reset-at",
        HeaderValue::from_static("1704069000"),
    );
    response
}

async fn read_next_sse_event(
    body: &mut (impl futures::Stream<Item = Result<Bytes, axum::Error>> + Unpin),
) -> Vec<u8> {
    let mut event = Vec::new();
    while !event.ends_with(b"\n\n") {
        let chunk = timeout(Duration::from_secs(5), body.next())
            .await
            .expect("forwarded stream made progress")
            .expect("stream continued")
            .expect("read forwarded stream chunk");
        event.extend(chunk);
    }
    event
}

fn test_state(upstream_addr: SocketAddr) -> ProxyState {
    ProxyState {
        client: reqwest::Client::new(),
        upstream_base: Url::parse(&format!("http://{upstream_addr}"))
            .expect("parse test upstream URL"),
        paying_account: PayingAccount {
            label: "Pool B".to_string(),
            access_token: "secondary-token".to_string(),
            account_id: "secondary-account".to_string(),
            usage: Arc::new(Mutex::new(None)),
            usage_store: None,
        },
        message_boundary: Arc::new(MessageBoundaryDetector::default()),
    }
}

async fn reply_with_usage() -> Response<Body> {
    let mut response = Response::new(Body::from("ok"));
    response.headers_mut().insert(
        "x-codex-primary-used-percent",
        HeaderValue::from_static("12.5"),
    );
    response.headers_mut().insert(
        "x-codex-primary-window-minutes",
        HeaderValue::from_static("300"),
    );
    response.headers_mut().insert(
        "x-codex-primary-reset-at",
        HeaderValue::from_static("1704069000"),
    );
    response
}

async fn capture_request(
    State(captured): State<Arc<Mutex<Option<CapturedRequest>>>>,
    request: Request<Body>,
) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, usize::MAX)
        .await
        .expect("read forwarded body")
        .to_vec();
    *captured.lock().expect("lock captured request") = Some(CapturedRequest {
        headers: parts.headers,
        body,
    });
    Response::new(Body::from("ok"))
}
