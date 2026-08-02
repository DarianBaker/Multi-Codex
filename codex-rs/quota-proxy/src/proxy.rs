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
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::Request;
use axum::http::Response;
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::header::HOST;
use futures::TryStreamExt;
use reqwest::Url;
use serde::Deserialize;

use crate::LoadedAccountCredentials;
use crate::PoolSettings;
use crate::usage::AccountUsage;
use crate::usage::UsageStore;

const CHATGPT_ACCOUNT_ID: &str = "chatgpt-account-id";
const X_CODEX_TURN_STATE: &str = "x-codex-turn-state";
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

#[derive(Default)]
struct HardRefusalReader {
    pending: Vec<u8>,
    recorded: bool,
}

impl HardRefusalReader {
    fn read(&mut self, chunk: &[u8], mut record: impl FnMut(i64)) {
        if self.recorded {
            return;
        }
        if self.pending.len() + chunk.len() > MAX_STREAMING_USAGE_EVENT_BYTES {
            self.pending.clear();
            self.recorded = true;
            return;
        }
        self.pending.extend_from_slice(chunk);
        if let Ok(response) = serde_json::from_slice::<HardRefusalResponse>(&self.pending)
            && response.error.kind == "usage_limit_reached"
        {
            self.recorded = true;
            record(response.error.resets_at);
        }
    }
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
        for candidate in &self.accounts {
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
        if profile.is_main {
            continue;
        }
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
        });
    }
    if candidates.is_empty() {
        return Err(anyhow!(
            "no usable secondary account; main account will not be used"
        ));
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
    let headers = end_to_end_headers(upstream.headers());
    record_reply_usage(&paying_account, &headers);

    // Keep the upstream body as a stream from socket to socket.
    let streaming_account = paying_account.clone();
    let mut streaming_usage = StreamingUsageReader::default();
    let mut hard_refusal =
        (status == StatusCode::TOO_MANY_REQUESTS).then(HardRefusalReader::default);
    let body = upstream.bytes_stream().inspect_ok(move |chunk| {
        streaming_usage.read(chunk, |usage| {
            record_account_usage(&streaming_account, usage);
        });
        if let Some(hard_refusal) = &mut hard_refusal {
            hard_refusal.read(chunk, |returns_at| {
                streaming_account.set_aside(SetAsideReason::OpenAiHardRefusal, returns_at);
            });
        }
    });
    let mut response = Response::new(Body::from_stream(body));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
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
