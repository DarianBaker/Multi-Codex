use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use axum::Router;
use axum::body::Body;
use axum::body::Bytes;
use axum::extract::FromRequestParts;
use axum::extract::State;
use axum::extract::ws::CloseFrame as AxumCloseFrame;
use axum::extract::ws::Message as AxumWebSocketMessage;
use axum::extract::ws::WebSocket;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::Request;
use axum::http::Response;
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::header::CONTENT_LENGTH;
use axum::http::header::HOST;
use axum::http::header::SEC_WEBSOCKET_EXTENSIONS;
use axum::response::IntoResponse;
use codex_utils_rustls_provider::ensure_rustls_crypto_provider;
use futures::SinkExt;
use futures::StreamExt;
use futures::TryStreamExt;
use reqwest::Url;
use serde::Deserialize;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as UpstreamWebSocketMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as UpstreamCloseFrame;

use crate::LoadedAccountCredentials;
use crate::PoolSettings;
use crate::usage::AccountUsage;
use crate::usage::UsageStore;
use crate::websocket_turn::WebsocketTurnTracker;
use crate::websocket_turn::X_CODEX_TURN_STATE;

const CHATGPT_ACCOUNT_ID: &str = "chatgpt-account-id";
const PRIMARY_RESET_AT: &str = "x-codex-primary-reset-at";
const PRIMARY_USED_PERCENT: &str = "x-codex-primary-used-percent";
const PRIMARY_WINDOW_MINUTES: &str = "x-codex-primary-window-minutes";
const MAX_STREAMING_USAGE_EVENT_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
struct StreamingUsageEvent {
    #[serde(rename = "type")]
    kind: String,
    rate_limits: Option<StreamingRateLimits>,
}

#[derive(Deserialize)]
struct StreamingRateLimits {
    primary: Option<StreamingUsageWindow>,
}

#[derive(Deserialize)]
struct StreamingUsageWindow {
    used_percent: f64,
    window_minutes: i64,
    reset_at: i64,
}

#[derive(Deserialize)]
struct HardRefusalResponse {
    error: HardRefusalError,
}

#[derive(Deserialize)]
struct HardRefusalError {
    #[serde(rename = "type")]
    kind: String,
    resets_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MessageRequestKind {
    NewMessage,
    FollowUp,
}

#[derive(Default)]
struct MessageBoundaryDetector;

impl MessageBoundaryDetector {
    fn classify(&self, headers: &HeaderMap) -> MessageRequestKind {
        if headers.contains_key(X_CODEX_TURN_STATE) {
            MessageRequestKind::FollowUp
        } else {
            MessageRequestKind::NewMessage
        }
    }
}

#[derive(Default)]
struct StreamingUsageReader {
    pending: Vec<u8>,
}

impl StreamingUsageReader {
    fn read(&mut self, chunk: &[u8], mut record: impl FnMut(AccountUsage)) {
        self.pending.extend_from_slice(chunk);
        loop {
            let lf_end = self
                .pending
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|position| position + 2);
            let crlf_end = self
                .pending
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| position + 4);
            let Some(event_end) = lf_end.into_iter().chain(crlf_end).min() else {
                break;
            };
            if event_end <= MAX_STREAMING_USAGE_EVENT_BYTES
                && let Ok(event_text) = std::str::from_utf8(&self.pending[..event_end])
                && let Some(data) = event_text
                    .lines()
                    .find_map(|line| line.strip_prefix("data:"))
                && let Ok(event) = serde_json::from_str::<StreamingUsageEvent>(data.trim_start())
                && event.kind == "codex.rate_limits"
                && let Some(primary) = event.rate_limits.and_then(|limits| limits.primary)
            {
                record(AccountUsage {
                    used_percent: primary.used_percent,
                    window_minutes: primary.window_minutes,
                    resets_at: primary.reset_at,
                });
            }
            self.pending.drain(..event_end);
        }
        if self.pending.len() > MAX_STREAMING_USAGE_EVENT_BYTES {
            self.pending.clear();
        }
    }
}

#[derive(Clone)]
struct PayingAccount {
    label: String,
    access_token: String,
    account_id: String,
    usage: Arc<Mutex<Option<AccountUsage>>>,
    usage_store: Option<Arc<UsageStore>>,
    set_aside: Arc<Mutex<Option<AccountSetAside>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AccountSetAside {
    reason: SetAsideReason,
    returns_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetAsideReason {
    OutOfQuota,
    OpenAiHardRefusal,
}

impl SetAsideReason {
    fn description(self) -> &'static str {
        match self {
            Self::OutOfQuota => "reported out of quota",
            Self::OpenAiHardRefusal => "OpenAI hard refusal",
        }
    }
}

impl PayingAccount {
    fn set_aside(&self, reason: SetAsideReason, returns_at: i64) {
        *self
            .set_aside
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(AccountSetAside { reason, returns_at });
        eprintln!(
            "set aside account '{}': {} until {} (Unix seconds)",
            self.label,
            reason.description(),
            returns_at
        );
    }
}

struct AccountCandidate {
    account: PayingAccount,
    priority: u32,
    switch_at_percent: f64,
    is_main: bool,
}

struct AccountSelector {
    accounts: Vec<AccountCandidate>,
}

impl AccountSelector {
    fn new(mut accounts: Vec<AccountCandidate>) -> Self {
        accounts.sort_by_key(|candidate| candidate.priority);
        Self { accounts }
    }

    fn select(&self) -> Result<PayingAccount> {
        let now = unix_now()?;
        self.select_at(now)
    }

    fn select_at(&self, now: i64) -> Result<PayingAccount> {
        let mut main: Option<&AccountCandidate> = None;
        for candidate in &self.accounts {
            if candidate.is_main {
                main = Some(candidate);
                continue;
            }
            let set_aside = *candidate
                .account
                .set_aside
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(set_aside) = set_aside
                && set_aside.returns_at > now
            {
                eprintln!(
                    "skipping set-aside account '{}': {}; returns at {} (Unix seconds)",
                    candidate.account.label,
                    set_aside.reason.description(),
                    set_aside.returns_at
                );
                continue;
            }
            if set_aside.is_some() {
                *candidate
                    .account
                    .set_aside
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
            let mut known_usage = candidate
                .account
                .usage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if known_usage.is_some_and(|usage| usage.resets_at <= now) {
                *known_usage = None;
            }
            let usage = *known_usage;
            drop(known_usage);
            match usage {
                Some(usage) if usage.used_percent >= candidate.switch_at_percent => {
                    eprintln!(
                        "skipping account '{}' at priority {}: {:.1}% used is at or above {:.1}% switch-over",
                        candidate.account.label,
                        candidate.priority,
                        usage.used_percent,
                        candidate.switch_at_percent
                    );
                }
                Some(usage) => {
                    eprintln!(
                        "selected account '{}' at priority {}: {:.1}% used is below {:.1}% switch-over",
                        candidate.account.label,
                        candidate.priority,
                        usage.used_percent,
                        candidate.switch_at_percent
                    );
                    if let Some(store) = &candidate.account.usage_store
                        && let Err(error) = store.set_paying_account(&candidate.account.label)
                    {
                        eprintln!("could not save paying account: {error}");
                    }
                    return Ok(candidate.account.clone());
                }
                None => {
                    eprintln!(
                        "selected account '{}' at priority {}: usage unavailable, treated as having room below {:.1}% switch-over",
                        candidate.account.label, candidate.priority, candidate.switch_at_percent
                    );
                    if let Some(store) = &candidate.account.usage_store
                        && let Err(error) = store.set_paying_account(&candidate.account.label)
                    {
                        eprintln!("could not save paying account: {error}");
                    }
                    return Ok(candidate.account.clone());
                }
            }
        }
        if let Some(candidate) = main {
            let set_aside = *candidate
                .account
                .set_aside
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(set_aside) = set_aside
                && set_aside.returns_at > now
            {
                eprintln!(
                    "skipping set-aside account '{}': {}; returns at {} (Unix seconds)",
                    candidate.account.label,
                    set_aside.reason.description(),
                    set_aside.returns_at
                );
                return Err(anyhow!(
                    "no account has room below its switch-over percentage"
                ));
            }
            if set_aside.is_some() {
                *candidate
                    .account
                    .set_aside
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
            let mut known_usage = candidate
                .account
                .usage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if known_usage.is_some_and(|usage| usage.resets_at <= now) {
                *known_usage = None;
            }
            drop(known_usage);
            eprintln!(
                "pool exhausted; main account '{}' is now paying",
                candidate.account.label
            );
            if let Some(store) = &candidate.account.usage_store
                && let Err(error) = store.set_paying_account(&candidate.account.label)
            {
                eprintln!("could not save paying account: {error}");
            }
            return Ok(candidate.account.clone());
        }
        Err(anyhow!(
            "no account has room below its switch-over percentage"
        ))
    }
}

#[derive(Default)]
struct MessageAccountPin {
    pinned: Mutex<Option<PayingAccount>>,
}

impl MessageAccountPin {
    fn account_for_request(
        &self,
        kind: MessageRequestKind,
        select: impl FnOnce() -> Result<PayingAccount>,
    ) -> Result<PayingAccount> {
        let mut pinned = self
            .pinned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if kind == MessageRequestKind::NewMessage {
            *pinned = None;
        }
        if pinned.is_none() {
            *pinned = Some(select()?);
        }
        pinned
            .as_ref()
            .cloned()
            .context("message has no paying account")
    }
}

#[derive(Clone)]
struct ProxyState {
    client: reqwest::Client,
    upstream_base: Url,
    account_selector: Arc<AccountSelector>,
    message_boundary: Arc<MessageBoundaryDetector>,
    account_pin: Arc<MessageAccountPin>,
}

/// Starts the transparent HTTP proxy and serves requests until it is stopped.
pub async fn serve(
    settings: &PoolSettings,
    accounts: Vec<LoadedAccountCredentials>,
    usage_path: PathBuf,
) -> Result<()> {
    let listen_addr: SocketAddr = settings
        .listen_addr
        .parse()
        .with_context(|| format!("listen_addr '{}' is invalid", settings.listen_addr))?;
    let upstream_base =
        Url::parse(&settings.upstream_base).context("upstream_base URL is invalid")?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("could not create upstream client")?;
    let now = unix_now()?;
    let loaded_usage = UsageStore::load(usage_path, now);
    if let Some(warning) = loaded_usage.warning {
        eprintln!("{warning}");
    }
    let usage_store = Arc::new(loaded_usage.store);
    let mut candidates = Vec::new();
    for account in accounts {
        let profile = settings
            .profiles
            .iter()
            .find(|profile| profile.label == account.label)
            .with_context(|| format!("loaded account '{}' is missing settings", account.label))?;
        let tokens = account
            .credentials
            .tokens
            .context("chosen account has no login tokens")?;
        let account_id = tokens
            .account_id
            .or(tokens.id_token.chatgpt_account_id)
            .context("chosen account has no account identifier")?;
        let usage = usage_store.get(&account.label);
        candidates.push(AccountCandidate {
            account: PayingAccount {
                label: account.label,
                access_token: tokens.access_token,
                account_id,
                usage: Arc::new(Mutex::new(usage)),
                usage_store: Some(Arc::clone(&usage_store)),
                set_aside: Arc::new(Mutex::new(None)),
            },
            priority: profile.priority,
            switch_at_percent: settings.switch_at_percent_for(profile),
            is_main: profile.is_main,
        });
    }
    if candidates.is_empty() {
        return Err(anyhow!("no usable account"));
    }
    let state = ProxyState {
        client,
        upstream_base,
        account_selector: Arc::new(AccountSelector::new(candidates)),
        message_boundary: Arc::new(MessageBoundaryDetector),
        account_pin: Arc::new(MessageAccountPin::default()),
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
    if request
        .headers()
        .get("upgrade")
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"))
    {
        let (mut parts, body) = request.into_parts();
        match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
            Ok(websocket) => {
                let request = Request::from_parts(parts, body);
                return match forward_websocket(&state, websocket, request).await {
                    Ok(response) => response,
                    Err(error) => {
                        eprintln!(
                            "websocket forwarding failed; Codex can fall back to HTTP: {error}"
                        );
                        let mut response =
                            Response::new(Body::from("websocket upstream unavailable"));
                        *response.status_mut() = StatusCode::UPGRADE_REQUIRED;
                        response
                    }
                };
            }
            Err(rejection) => return rejection.into_response(),
        }
    }
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

async fn forward_websocket(
    state: &ProxyState,
    websocket: WebSocketUpgrade,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let (parts, _) = request.into_parts();
    let kind = state.message_boundary.classify(&parts.headers);
    let paying_account = state
        .account_pin
        .account_for_request(kind, || state.account_selector.select())?;
    eprintln!(
        "websocket connection paid by account '{}'",
        paying_account.label
    );
    let request_target = parts
        .uri
        .path_and_query()
        .map_or("/", |value| value.as_str());
    let mut upstream_url = upstream_url(&state.upstream_base, request_target)?;
    match upstream_url.scheme() {
        "http" => upstream_url
            .set_scheme("ws")
            .map_err(|()| anyhow!("upstream URL cannot use WebSocket transport"))?,
        "https" => upstream_url
            .set_scheme("wss")
            .map_err(|()| anyhow!("upstream URL cannot use WebSocket transport"))?,
        "ws" | "wss" => {}
        _ => return Err(anyhow!("upstream URL cannot use WebSocket transport")),
    }
    let upstream =
        connect_upstream_websocket(&upstream_url, &parts.headers, &paying_account).await?;
    let relay_state = state.clone();
    Ok(websocket
        .on_upgrade(move |downstream| {
            relay_websocket(
                downstream,
                upstream,
                relay_state,
                upstream_url,
                parts.headers,
                paying_account,
            )
        })
        .into_response())
}

async fn connect_upstream_websocket(
    upstream_url: &Url,
    headers: &HeaderMap,
    paying_account: &PayingAccount,
) -> Result<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>> {
    ensure_rustls_crypto_provider();

    let mut upstream_request = upstream_url
        .as_str()
        .into_client_request()
        .context("could not build upstream websocket request")?;
    for (name, value) in headers {
        if name == SEC_WEBSOCKET_EXTENSIONS {
            continue;
        }
        upstream_request
            .headers_mut()
            .insert(name.clone(), value.clone());
    }
    upstream_request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", paying_account.access_token))
            .context("chosen account access token is invalid")?,
    );
    upstream_request.headers_mut().insert(
        CHATGPT_ACCOUNT_ID,
        HeaderValue::from_str(&paying_account.account_id)
            .context("chosen account identifier is invalid")?,
    );
    upstream_request
        .headers_mut()
        .insert(HOST, upstream_host(upstream_url)?);

    let (upstream, _) = connect_async(upstream_request)
        .await
        .context("upstream websocket connection failed")?;
    Ok(upstream)
}

async fn relay_websocket(
    mut downstream: WebSocket,
    mut upstream: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    state: ProxyState,
    upstream_url: Url,
    headers: HeaderMap,
    mut paying_account: PayingAccount,
) {
    let turn_tracker = WebsocketTurnTracker;
    loop {
        tokio::select! {
            message = downstream.next() => {
                let Some(Ok(message)) = message else {
                    break;
                };
                if let AxumWebSocketMessage::Text(text) = &message
                    && turn_tracker.starts_new_turn(text)
                {
                    let next_account = match state.account_pin.account_for_request(
                        MessageRequestKind::NewMessage,
                        || state.account_selector.select(),
                    ) {
                        Ok(account) => account,
                        Err(error) => {
                            eprintln!("could not select account for new websocket turn: {error}");
                            break;
                        }
                    };
                    if next_account.account_id != paying_account.account_id {
                        upstream = match connect_upstream_websocket(
                            &upstream_url,
                            &headers,
                            &next_account,
                        )
                        .await
                        {
                            Ok(upstream) => upstream,
                            Err(error) => {
                                eprintln!("could not switch websocket account: {error}");
                                break;
                            }
                        };
                    }
                    paying_account = next_account;
                    eprintln!(
                        "new websocket turn paid by account '{}'",
                        paying_account.label
                    );
                }
                if upstream.send(to_upstream_message(message)).await.is_err() {
                    break;
                }
            }
            message = upstream.next() => {
                let Some(Ok(message)) = message else {
                    break;
                };
                let Some(message) = to_downstream_message(message) else {
                    continue;
                };
                if downstream.send(message).await.is_err() {
                    break;
                }
            }
        }
    }
}

fn to_upstream_message(message: AxumWebSocketMessage) -> UpstreamWebSocketMessage {
    match message {
        AxumWebSocketMessage::Text(text) => UpstreamWebSocketMessage::Text(text.to_string().into()),
        AxumWebSocketMessage::Binary(data) => UpstreamWebSocketMessage::Binary(data),
        AxumWebSocketMessage::Ping(data) => UpstreamWebSocketMessage::Ping(data),
        AxumWebSocketMessage::Pong(data) => UpstreamWebSocketMessage::Pong(data),
        AxumWebSocketMessage::Close(frame) => {
            UpstreamWebSocketMessage::Close(frame.map(|frame| UpstreamCloseFrame {
                code: frame.code.into(),
                reason: frame.reason.to_string().into(),
            }))
        }
    }
}

fn to_downstream_message(message: UpstreamWebSocketMessage) -> Option<AxumWebSocketMessage> {
    match message {
        UpstreamWebSocketMessage::Text(text) => {
            Some(AxumWebSocketMessage::Text(text.to_string().into()))
        }
        UpstreamWebSocketMessage::Binary(data) => Some(AxumWebSocketMessage::Binary(data)),
        UpstreamWebSocketMessage::Ping(data) => Some(AxumWebSocketMessage::Ping(data)),
        UpstreamWebSocketMessage::Pong(data) => Some(AxumWebSocketMessage::Pong(data)),
        UpstreamWebSocketMessage::Close(frame) => {
            Some(AxumWebSocketMessage::Close(frame.map(|frame| {
                AxumCloseFrame {
                    code: frame.code.into(),
                    reason: frame.reason.to_string().into(),
                }
            })))
        }
        UpstreamWebSocketMessage::Frame(_) => None,
    }
}

async fn forward_request(state: &ProxyState, request: Request<Body>) -> Result<Response<Body>> {
    let (parts, body) = request.into_parts();
    let kind = state.message_boundary.classify(&parts.headers);
    match kind {
        MessageRequestKind::NewMessage => eprintln!("request starts a new message"),
        MessageRequestKind::FollowUp => eprintln!("request continues the current message"),
    }
    let paying_account = state
        .account_pin
        .account_for_request(kind, || state.account_selector.select())?;
    eprintln!("request paid by account '{}'", paying_account.label);
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
        HeaderValue::from_str(&format!("Bearer {}", paying_account.access_token))
            .context("chosen account access token is invalid")?,
    );
    headers.insert(
        CHATGPT_ACCOUNT_ID,
        HeaderValue::from_str(&paying_account.account_id)
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
    let mut headers = end_to_end_headers(upstream.headers());
    record_reply_usage(&paying_account, &headers);

    let (body, rewritten_length) = if status == StatusCode::TOO_MANY_REQUESTS {
        hard_refusal_body(&paying_account, upstream).await?
    } else {
        // Keep successful upstream replies streaming from socket to socket.
        let streaming_account = paying_account.clone();
        let mut streaming_usage = StreamingUsageReader::default();
        (
            Body::from_stream(upstream.bytes_stream().inspect_ok(move |chunk| {
                streaming_usage.read(chunk, |usage| {
                    record_account_usage(&streaming_account, usage);
                });
            })),
            None,
        )
    };
    if let Some(rewritten_length) = rewritten_length {
        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&rewritten_length.to_string())
                .context("rewritten response length is invalid")?,
        );
    }
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

async fn hard_refusal_body(
    account: &PayingAccount,
    upstream: reqwest::Response,
) -> Result<(Body, Option<usize>)> {
    let mut stream = upstream.bytes_stream();
    let mut pending = Vec::new();
    while let Some(chunk) = stream.try_next().await.context("upstream request failed")? {
        if pending.len() + chunk.len() > MAX_STREAMING_USAGE_EVENT_BYTES {
            let initial = futures::stream::iter([
                Ok::<_, reqwest::Error>(Bytes::from(pending)),
                Ok::<_, reqwest::Error>(chunk),
            ]);
            return Ok((Body::from_stream(initial.chain(stream)), None));
        }
        pending.extend_from_slice(&chunk);
    }

    let Ok(refusal) = serde_json::from_slice::<HardRefusalResponse>(&pending) else {
        return Ok((Body::from(pending), None));
    };
    if refusal.error.kind != "usage_limit_reached" {
        return Ok((Body::from(pending), None));
    }
    account.set_aside(SetAsideReason::OpenAiHardRefusal, refusal.error.resets_at);
    let message = format!(
        "Account '{}' ran out of quota mid-answer. Retry the same message; the next available account will be used.",
        account.label
    );
    eprintln!("{message}");
    let mut response: serde_json::Value =
        serde_json::from_slice(&pending).context("could not parse OpenAI hard refusal")?;
    response["error"]["message"] = serde_json::Value::String(message);
    let body = serde_json::to_vec(&response).context("could not rewrite OpenAI hard refusal")?;
    let length = body.len();
    Ok((Body::from(body), Some(length)))
}

fn record_reply_usage(account: &PayingAccount, headers: &HeaderMap) {
    let Some(usage) = reply_usage(headers) else {
        return;
    };
    record_account_usage(account, usage);
}

fn record_account_usage(account: &PayingAccount, usage: AccountUsage) {
    let mut known_usage = account
        .usage
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *known_usage = Some(usage);
    drop(known_usage);
    if usage.used_percent >= 100.0 {
        account.set_aside(SetAsideReason::OutOfQuota, usage.resets_at);
    }
    if let Some(usage_store) = &account.usage_store
        && let Err(error) = usage_store.record(&account.label, usage)
    {
        eprintln!(
            "could not save usage for account '{}': {error}",
            account.label
        );
    }
}

fn unix_now() -> Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time is before the Unix epoch")?
        .as_secs() as i64)
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
