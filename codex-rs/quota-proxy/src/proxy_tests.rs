use std::sync::Arc;
use std::sync::Mutex;

use axum::Router;
use axum::body::Body;
use axum::body::to_bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::Request;
use axum::http::Response;
use axum::routing::post;
use pretty_assertions::assert_eq;

use super::*;

#[derive(Debug)]
struct CapturedRequest {
    headers: HeaderMap,
    body: Vec<u8>,
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
        paying_account: PayingAccount {
            label: "Pool B".to_string(),
            access_token: "secondary-token".to_string(),
            account_id: "secondary-account".to_string(),
        },
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
