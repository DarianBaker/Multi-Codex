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
use futures::SinkExt;
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
async fn websocket_reconnect_keeps_same_turn_account_and_new_turn_may_switch() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let handshake_accounts = Arc::new(Mutex::new(Vec::new()));
    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind websocket upstream");
    let upstream_addr = upstream_listener
        .local_addr()
        .expect("read websocket upstream address");
    let upstream_received = Arc::clone(&received);
    let upstream_handshake_accounts = Arc::clone(&handshake_accounts);
    let upstream_task = tokio::spawn(async move {
        loop {
            let (stream, _) = upstream_listener
                .accept()
                .await
                .expect("accept websocket upstream connection");
            let connection_account = Arc::new(Mutex::new(None));
            let handshake_account = Arc::clone(&connection_account);
            let connection_received = Arc::clone(&upstream_received);
            let connection_handshake_accounts = Arc::clone(&upstream_handshake_accounts);
            tokio::spawn(async move {
                let mut websocket = tokio_tungstenite::accept_hdr_async(
                    stream,
                    move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                          response| {
                        *handshake_account.lock().expect("lock handshake account") = request
                            .headers()
                            .get(CHATGPT_ACCOUNT_ID)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        Ok(response)
                    },
                )
                .await
                .expect("accept websocket handshake");
                let account = connection_account
                    .lock()
                    .expect("lock connection account")
                    .clone()
                    .expect("account header on websocket handshake");
                connection_handshake_accounts
                    .lock()
                    .expect("lock websocket handshake accounts")
                    .push(account.clone());
                while let Some(message) = websocket.next().await {
                    let Ok(message) = message else {
                        break;
                    };
                    let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
                        continue;
                    };
                    let body: serde_json::Value =
                        serde_json::from_str(&text).expect("parse websocket request");
                    let turn_id = body["client_metadata"]["turn_id"]
                        .as_str()
                        .expect("turn id in websocket request")
                        .to_string();
                    connection_received
                        .lock()
                        .expect("lock received websocket requests")
                        .push((account.clone(), turn_id));
                    websocket
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            r#"{"type":"response.completed","response":{"id":"response"}}"#.into(),
                        ))
                        .await
                        .expect("send websocket response");
                }
            });
        }
    });

    let first_usage = Arc::new(Mutex::new(None));
    let state = ProxyState {
        client: reqwest::Client::new(),
        upstream_base: Url::parse(&format!("http://{upstream_addr}"))
            .expect("parse websocket upstream URL"),
        account_selector: Arc::new(AccountSelector::new(vec![
            AccountCandidate {
                account: PayingAccount {
                    label: "first".to_string(),
                    access_token: "first-token".to_string(),
                    account_id: "first-account".to_string(),
                    usage: Arc::clone(&first_usage),
                    usage_store: None,
                    set_aside: Arc::new(Mutex::new(None)),
                },
                priority: 1,
                switch_at_percent: 80.0,
                is_main: false,
            },
            AccountCandidate {
                account: PayingAccount {
                    label: "second".to_string(),
                    access_token: "second-token".to_string(),
                    account_id: "second-account".to_string(),
                    usage: Arc::new(Mutex::new(None)),
                    usage_store: None,
                    set_aside: Arc::new(Mutex::new(None)),
                },
                priority: 2,
                switch_at_percent: 80.0,
                is_main: false,
            },
        ])),
        message_boundary: Arc::new(MessageBoundaryDetector),
        account_pin: Arc::new(MessageAccountPin::default()),
    };
    let proxy = Router::new()
        .route("/responses", axum::routing::any(forward))
        .with_state(state);
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind websocket proxy");
    let proxy_addr = proxy_listener
        .local_addr()
        .expect("read websocket proxy address");
    let proxy_task = tokio::spawn(async move {
        axum::serve(proxy_listener, proxy)
            .await
            .expect("serve websocket proxy");
    });
    let (mut websocket, _) =
        tokio_tungstenite::connect_async(format!("ws://{proxy_addr}/responses"))
            .await
            .expect("connect through websocket proxy");
    assert_eq!(
        *handshake_accounts
            .lock()
            .expect("lock websocket handshake accounts"),
        Vec::<String>::new()
    );

    for (input_type, previous_response_id, turn_state) in [
        ("message", None, None),
        (
            "function_call_output",
            Some("response-1"),
            Some("same-turn"),
        ),
        (
            "custom_tool_call_output",
            Some("response-2"),
            Some("same-turn"),
        ),
        ("message", Some("response-3"), None),
    ] {
        if input_type == "function_call_output" {
            *first_usage.lock().expect("lock first usage") = Some(AccountUsage {
                used_percent: 90.0,
                window_minutes: 300,
                resets_at: 4_102_444_800,
            });
        }
        if previous_response_id.is_some() {
            websocket.close(None).await.expect("close websocket client");
            (websocket, _) =
                tokio_tungstenite::connect_async(format!("ws://{proxy_addr}/responses"))
                    .await
                    .expect("reconnect through websocket proxy");
        }
        let mut request = json!({
            "type": "response.create",
            "client_metadata": {"turn_id": "unchanged-secondary-field"},
            "input": [{"type": input_type}],
        });
        if let Some(previous_response_id) = previous_response_id {
            request["previous_response_id"] = json!(previous_response_id);
        }
        if let Some(turn_state) = turn_state {
            request["client_metadata"][X_CODEX_TURN_STATE] = json!(turn_state);
        }
        websocket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                request.to_string().into(),
            ))
            .await
            .expect("send websocket request");
        timeout(Duration::from_secs(5), websocket.next())
            .await
            .expect("websocket response arrived")
            .expect("websocket remained open")
            .expect("read websocket response");
    }

    assert_eq!(
        *received.lock().expect("lock received websocket requests"),
        vec![
            (
                "first-account".to_string(),
                "unchanged-secondary-field".to_string(),
            ),
            (
                "first-account".to_string(),
                "unchanged-secondary-field".to_string(),
            ),
            (
                "first-account".to_string(),
                "unchanged-secondary-field".to_string(),
            ),
            (
                "second-account".to_string(),
                "unchanged-secondary-field".to_string(),
            ),
        ]
    );
    websocket.close(None).await.expect("close websocket client");
    proxy_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn websocket_extensions_are_not_forwarded_to_the_upstream_handshake() {
    let upstream_extensions = Arc::new(Mutex::new(None));
    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind websocket upstream");
    let upstream_addr = upstream_listener
        .local_addr()
        .expect("read websocket upstream address");
    let captured_extensions = Arc::clone(&upstream_extensions);
    let upstream_task = tokio::spawn(async move {
        let (stream, _) = upstream_listener
            .accept()
            .await
            .expect("accept websocket upstream connection");
        let mut websocket = tokio_tungstenite::accept_hdr_async(
            stream,
            move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                  response| {
                *captured_extensions
                    .lock()
                    .expect("lock captured websocket extensions") =
                    request.headers().get("sec-websocket-extensions").cloned();
                Ok(response)
            },
        )
        .await
        .expect("accept websocket handshake");
        let message = websocket
            .next()
            .await
            .expect("receive websocket frame")
            .expect("read websocket frame");
        websocket.send(message).await.expect("echo websocket frame");
    });

    let proxy = Router::new()
        .route("/responses", axum::routing::any(forward))
        .with_state(test_state(upstream_addr));
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind websocket proxy");
    let proxy_addr = proxy_listener
        .local_addr()
        .expect("read websocket proxy address");
    let proxy_task = tokio::spawn(async move {
        axum::serve(proxy_listener, proxy)
            .await
            .expect("serve websocket proxy");
    });
    let mut request = format!("ws://{proxy_addr}/responses")
        .into_client_request()
        .expect("build downstream websocket request");
    request.headers_mut().insert(
        "sec-websocket-extensions",
        HeaderValue::from_static("permessage-deflate; client_max_window_bits"),
    );
    let (mut websocket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("connect through websocket proxy");
    let response_create = r#"{"type":"response.create","client_metadata":{},"input":[]}"#;
    websocket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            response_create.into(),
        ))
        .await
        .expect("send downstream frame");
    let echoed = websocket
        .next()
        .await
        .expect("receive echoed frame")
        .expect("read echoed frame");

    assert_eq!(
        (
            upstream_extensions
                .lock()
                .expect("lock upstream websocket extensions")
                .clone(),
            echoed,
        ),
        (
            None,
            tokio_tungstenite::tungstenite::Message::Text(response_create.into()),
        )
    );
    websocket.close(None).await.expect("close websocket client");
    proxy_task.abort();
    upstream_task.await.expect("join websocket upstream");
}

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
        account_selector: Arc::new(AccountSelector::new(vec![AccountCandidate {
            account: PayingAccount {
                label: "Pool B".to_string(),
                access_token: "secondary-token".to_string(),
                account_id: "secondary-account".to_string(),
                usage: Arc::new(Mutex::new(None)),
                usage_store: None,
                set_aside: Arc::new(Mutex::new(None)),
            },
            priority: 1,
            switch_at_percent: 80.0,
            is_main: false,
        }])),
        message_boundary: Arc::new(MessageBoundaryDetector),
        account_pin: Arc::new(MessageAccountPin::default()),
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
    Arc::get_mut(&mut state.account_selector)
        .expect("test owns account selector")
        .accounts[0]
        .account
        .usage_store = Some(Arc::new(UsageStore::load(&usage_path, 0).store));
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
            state.account_selector.accounts[0].account.label.as_str(),
            *state.account_selector.accounts[0]
                .account
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
            "paying_account": "Pool B",
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
    *state.account_selector.accounts[0]
        .account
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
        *state.account_selector.accounts[0]
            .account
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
        *state.account_selector.accounts[0]
            .account
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
        *state.account_selector.accounts[0]
            .account
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

#[tokio::test]
async fn mid_answer_limit_explains_retry_and_retry_uses_next_account_without_changing_history() {
    let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let app = Router::new().route(
        "/responses",
        post({
            let captured = Arc::clone(&captured);
            move |request: Request<Body>| {
                let captured = Arc::clone(&captured);
                async move {
                    let (parts, body) = request.into_parts();
                    let body = to_bytes(body, usize::MAX)
                        .await
                        .expect("read forwarded body")
                        .to_vec();
                    let mut captured = captured.lock().expect("lock captured requests");
                    captured.push(CapturedRequest {
                        headers: parts.headers,
                        body,
                    });
                    if captured.len() == 2 {
                        let mut response = Response::new(Body::from(
                            r#"{"error":{"type":"usage_limit_reached","message":"upstream limit","resets_at":4102444800}}"#,
                        ));
                        *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
                        response
                    } else {
                        Response::new(Body::from(if captured.len() == 1 {
                            "tool requested"
                        } else {
                            "retry succeeded"
                        }))
                    }
                }
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

    let first = PayingAccount {
        label: "Pool B".to_string(),
        access_token: "first-token".to_string(),
        account_id: "first-account".to_string(),
        usage: Arc::new(Mutex::new(None)),
        usage_store: None,
        set_aside: Arc::new(Mutex::new(None)),
    };
    let second = PayingAccount {
        label: "Pool C".to_string(),
        access_token: "second-token".to_string(),
        account_id: "second-account".to_string(),
        usage: Arc::new(Mutex::new(None)),
        usage_store: None,
        set_aside: Arc::new(Mutex::new(None)),
    };
    let state = ProxyState {
        client: reqwest::Client::new(),
        upstream_base: Url::parse(&format!("http://{upstream_addr}"))
            .expect("parse test upstream URL"),
        account_selector: Arc::new(AccountSelector::new(vec![
            AccountCandidate {
                account: first.clone(),
                priority: 1,
                switch_at_percent: 80.0,
                is_main: false,
            },
            AccountCandidate {
                account: second,
                priority: 2,
                switch_at_percent: 80.0,
                is_main: false,
            },
        ])),
        message_boundary: Arc::new(MessageBoundaryDetector),
        account_pin: Arc::new(MessageAccountPin::default()),
    };
    let message = br#"{"input":[{"role":"user","content":"keep history exactly"}]}"#;
    let tool_output =
        br#"{"input":[{"type":"function_call_output","call_id":"call-1","output":"unchanged"}]}"#;

    let initial_response = forward_request(
        &state,
        Request::builder()
            .method("POST")
            .uri("/responses")
            .body(Body::from(message.as_slice()))
            .expect("build first request"),
    )
    .await
    .expect("forward initial request");
    assert_eq!(initial_response.status(), StatusCode::OK);
    to_bytes(initial_response.into_body(), usize::MAX)
        .await
        .expect("read initial response body");

    let limit_response = forward_request(
        &state,
        Request::builder()
            .method("POST")
            .uri("/responses")
            .header("x-codex-turn-state", "same-turn")
            .body(Body::from(tool_output.as_slice()))
            .expect("build follow-up request"),
    )
    .await
    .expect("forward follow-up request");
    let limit_content_length = limit_response
        .headers()
        .get("content-length")
        .expect("limit response content length")
        .to_str()
        .expect("content length is text")
        .parse::<usize>()
        .expect("content length is numeric");
    let limit_body = to_bytes(limit_response.into_body(), usize::MAX)
        .await
        .expect("read limit response body");

    let retry_response = forward_request(
        &state,
        Request::builder()
            .method("POST")
            .uri("/responses")
            .body(Body::from(message.as_slice()))
            .expect("build retry request"),
    )
    .await
    .expect("forward retry request");
    let retry_status = retry_response.status();
    let retry_body = to_bytes(retry_response.into_body(), usize::MAX)
        .await
        .expect("read retry response body");
    let limit_json: serde_json::Value =
        serde_json::from_slice(&limit_body).expect("parse limit response body");
    let limit_message = limit_json["error"]["message"].as_str();
    let captured = captured.lock().expect("lock captured requests");

    assert_eq!(
        (
            limit_message,
            *first.set_aside.lock().expect("lock first set-aside state"),
            captured
                .iter()
                .map(|request| request.body.as_slice())
                .collect::<Vec<_>>(),
            captured
                .iter()
                .map(|request| request.headers.get(AUTHORIZATION))
                .collect::<Vec<_>>(),
            retry_status,
            retry_body.as_ref(),
            limit_content_length,
        ),
        (
            Some(
                "Account 'Pool B' ran out of quota mid-answer. Retry the same message; the next available account will be used."
            ),
            Some(AccountSetAside {
                reason: SetAsideReason::OpenAiHardRefusal,
                returns_at: 4_102_444_800,
            }),
            vec![
                message.as_slice(),
                tool_output.as_slice(),
                message.as_slice()
            ],
            vec![
                Some(&HeaderValue::from_static("Bearer first-token")),
                Some(&HeaderValue::from_static("Bearer first-token")),
                Some(&HeaderValue::from_static("Bearer second-token")),
            ],
            StatusCode::OK,
            b"retry succeeded".as_slice(),
            limit_body.len(),
        )
    );
}

#[test]
fn message_with_several_tool_steps_keeps_one_boundary_for_normal_and_streaming_requests() {
    let detector = MessageBoundaryDetector;
    let mut follow_up_headers = HeaderMap::new();
    follow_up_headers.insert("x-codex-turn-state", HeaderValue::from_static("same-turn"));
    let requests = [
        (
            HeaderMap::new(),
            json!({
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": "start"
                }],
                "stream": false
            }),
        ),
        (
            follow_up_headers.clone(),
            json!({
                "input": [{
                    "type": "function_call_output",
                    "call_id": "call-1",
                    "output": "first"
                }],
                "stream": false
            }),
        ),
        (
            follow_up_headers.clone(),
            json!({
                "input": [{
                    "type": "custom_tool_call_output",
                    "call_id": "call-2",
                    "output": "second"
                }],
                "stream": true
            }),
        ),
        (
            follow_up_headers,
            json!({
                "input": [{
                    "type": "tool_search_output",
                    "call_id": "call-3",
                    "status": "completed",
                    "tools": []
                }],
                "stream": true
            }),
        ),
        (
            HeaderMap::new(),
            json!({
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": "next"
                }],
                "stream": true
            }),
        ),
    ];

    let actual = requests
        .iter()
        .map(|(headers, body)| {
            (
                detector.classify(headers),
                body["stream"].as_bool().expect("request stream flag"),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        actual,
        vec![
            (MessageRequestKind::NewMessage, false),
            (MessageRequestKind::FollowUp("same-turn".to_string()), false),
            (MessageRequestKind::FollowUp("same-turn".to_string()), true),
            (MessageRequestKind::FollowUp("same-turn".to_string()), true),
            (MessageRequestKind::NewMessage, true),
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
        account_selector: Arc::new(AccountSelector::new(vec![AccountCandidate {
            account: PayingAccount {
                label: "Pool B".to_string(),
                access_token: "secondary-token".to_string(),
                account_id: "secondary-account".to_string(),
                usage: Arc::new(Mutex::new(None)),
                usage_store: None,
                set_aside: Arc::new(Mutex::new(None)),
            },
            priority: 1,
            switch_at_percent: 80.0,
            is_main: false,
        }])),
        message_boundary: Arc::new(MessageBoundaryDetector),
        account_pin: Arc::new(MessageAccountPin::default()),
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
