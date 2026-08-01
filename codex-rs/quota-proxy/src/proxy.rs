use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::Request;
use axum::http::Response;
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::header::HOST;
use reqwest::Url;

use crate::LoadedAccountCredentials;

const CHATGPT_ACCOUNT_ID: &str = "chatgpt-account-id";
const PRIMARY_RESET_AT: &str = "x-codex-primary-reset-at";
const PRIMARY_USED_PERCENT: &str = "x-codex-primary-used-percent";
const PRIMARY_WINDOW_MINUTES: &str = "x-codex-primary-window-minutes";

#[derive(Clone, Copy, Debug, PartialEq)]
struct AccountUsage {
    used_percent: f64,
    window_minutes: i64,
    resets_at: i64,
}

#[derive(Clone)]
struct PayingAccount {
    label: String,
    access_token: String,
    account_id: String,
    usage: Arc<Mutex<Option<AccountUsage>>>,
}

#[derive(Clone)]
struct ProxyState {
    client: reqwest::Client,
    upstream_base: Url,
    paying_account: PayingAccount,
}

/// Starts the transparent HTTP proxy and serves requests until it is stopped.
pub async fn serve(
    listen_addr: &str,
    upstream_base: &str,
    account: LoadedAccountCredentials,
) -> Result<()> {
    let listen_addr: SocketAddr = listen_addr
        .parse()
        .with_context(|| format!("listen_addr '{listen_addr}' is invalid"))?;
    let upstream_base = Url::parse(upstream_base).context("upstream_base URL is invalid")?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("could not create upstream client")?;
    let tokens = account
        .credentials
        .tokens
        .context("chosen account has no login tokens")?;
    let account_id = tokens
        .account_id
        .or(tokens.id_token.chatgpt_account_id)
        .context("chosen account has no account identifier")?;
    let state = ProxyState {
        client,
        upstream_base,
        paying_account: PayingAccount {
            label: account.label,
            access_token: tokens.access_token,
            account_id,
            usage: Arc::new(Mutex::new(None)),
        },
    };
    let app = Router::new().fallback(forward).with_state(state);
    let listener = tokio::net::TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("could not listen on {listen_addr}"))?;
    let bound_addr = listener
        .local_addr()
        .context("could not read proxy listen address")?;

    eprintln!("codex-quota-proxy listening on {bound_addr}");
    axum::serve(listener, app)
        .await
        .context("proxy stopped unexpectedly")
}

async fn forward(State(state): State<ProxyState>, request: Request<Body>) -> Response<Body> {
    eprintln!("request paid by account '{}'", state.paying_account.label);
    match forward_request(&state, request).await {
        Ok(response) => response,
        Err(error) => {
            eprintln!("forwarding failed: {error}");
            let mut response = Response::new(Body::from("upstream request failed"));
            *response.status_mut() = StatusCode::BAD_GATEWAY;
            response
        }
    }
}

async fn forward_request(state: &ProxyState, request: Request<Body>) -> Result<Response<Body>> {
    let (parts, body) = request.into_parts();
    let request_target = parts
        .uri
        .path_and_query()
        .map_or("/", |value| value.as_str());
    let upstream_url = upstream_url(&state.upstream_base, request_target)?;
    let mut headers = end_to_end_headers(&parts.headers);
    // Replace only the two headers that select the paying account.
    headers.remove(AUTHORIZATION);
    headers.remove(CHATGPT_ACCOUNT_ID);
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", state.paying_account.access_token))
            .context("chosen account access token is invalid")?,
    );
    headers.insert(
        CHATGPT_ACCOUNT_ID,
        HeaderValue::from_str(&state.paying_account.account_id)
            .context("chosen account identifier is invalid")?,
    );
    headers.insert(HOST, upstream_host(&state.upstream_base)?);

    let upstream = state
        .client
        .request(parts.method, upstream_url)
        .headers(headers)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()))
        .send()
        .await
        .context("upstream request failed")?;
    let status = upstream.status();
    let headers = end_to_end_headers(upstream.headers());
    record_reply_usage(&state.paying_account, &headers);

    // Keep the upstream body as a stream from socket to socket.
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

fn record_reply_usage(account: &PayingAccount, headers: &HeaderMap) {
    let Some(usage) = reply_usage(headers) else {
        return;
    };
    let mut known_usage = account
        .usage
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *known_usage = Some(usage);
}

fn reply_usage(headers: &HeaderMap) -> Option<AccountUsage> {
    Some(AccountUsage {
        used_percent: headers
            .get(PRIMARY_USED_PERCENT)?
            .to_str()
            .ok()?
            .parse()
            .ok()?,
        window_minutes: headers
            .get(PRIMARY_WINDOW_MINUTES)?
            .to_str()
            .ok()?
            .parse()
            .ok()?,
        resets_at: headers.get(PRIMARY_RESET_AT)?.to_str().ok()?.parse().ok()?,
    })
}

fn upstream_url(base: &Url, request_target: &str) -> Result<Url> {
    let parsed = Url::parse(&format!("http://localhost{request_target}"))
        .context("request target is invalid")?;
    let mut target = base.clone();
    target.set_path(parsed.path());
    target.set_query(parsed.query());
    target.set_fragment(None);
    Ok(target)
}

fn upstream_host(base: &Url) -> Result<HeaderValue> {
    let host = base
        .host_str()
        .ok_or_else(|| anyhow!("upstream_base must contain a host"))?;
    let host = match base.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    HeaderValue::from_str(&host).context("upstream host is invalid")
}

fn end_to_end_headers(headers: &HeaderMap) -> HeaderMap {
    headers
        .iter()
        .filter(|(name, _)| !is_hop_by_hop(name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
