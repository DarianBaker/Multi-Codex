use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;

use crate::PoolSettings;
use crate::proxy::serve_ephemeral;
use crate::usage::AccountUsage;
use crate::usage::UsageStore;
use crate::usage_wire::verify_usage_reading;

const OVERALL_BUDGET: Duration = Duration::from_secs(50);
const PER_CALL_TIMEOUT: Duration = Duration::from_secs(15);
const USAGE_PATH: &str = "/api/codex/usage";

/// Outcome of one of `selfcheck`'s three named checks.
pub(crate) enum CheckOutcome {
    Ok(String),
    Broken(String),
    Skipped(String),
}

impl CheckOutcome {
    fn tag(&self) -> &'static str {
        match self {
            Self::Ok(_) => "OK",
            Self::Broken(_) => "BROKEN",
            Self::Skipped(_) => "SKIPPED",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Ok(detail) | Self::Broken(detail) | Self::Skipped(detail) => detail,
        }
    }

    fn is_broken(&self) -> bool {
        matches!(self, Self::Broken(_))
    }
}

pub struct SelfcheckReport {
    pub(crate) redirection: CheckOutcome,
    pub(crate) account_swap: CheckOutcome,
    pub(crate) usage_reading: CheckOutcome,
    pub(crate) elapsed: Duration,
}

impl SelfcheckReport {
    pub fn passed(&self) -> bool {
        !self.redirection.is_broken()
            && !self.account_swap.is_broken()
            && !self.usage_reading.is_broken()
    }

    pub fn render(&self) -> String {
        let broken_count = [&self.redirection, &self.account_swap, &self.usage_reading]
            .into_iter()
            .filter(|outcome| outcome.is_broken())
            .count();
        format!(
            "REDIRECTION: {} ({})\nACCOUNT SWAP: {} ({})\nUSAGE READING: {} ({})\nSELFCHECK: {} ({} of 3 checks broken, {:.1}s)\n",
            self.redirection.tag(),
            self.redirection.detail(),
            self.account_swap.tag(),
            self.account_swap.detail(),
            self.usage_reading.tag(),
            self.usage_reading.detail(),
            if self.passed() { "PASS" } else { "FAIL" },
            broken_count,
            self.elapsed.as_secs_f64(),
        )
    }
}

/// Runs the post-upgrade health check: does redirection still reach the real
/// upstream, does account swapping still pick a different account once the
/// first is over its threshold, and does usage-reading still report one row
/// per configured account plus the pool total. Everything runs against a
/// throwaway usage file and ephemeral, OS-assigned ports so it never
/// interferes with a real already-running proxy or its recorded state.
pub async fn run(settings_path: &Path) -> Result<SelfcheckReport> {
    let start = SystemTime::now();
    let result = tokio::time::timeout(OVERALL_BUDGET, run_checks(settings_path)).await;
    let elapsed = start.elapsed().unwrap_or(Duration::ZERO);
    match result {
        Ok(Ok(mut report)) => {
            report.elapsed = elapsed;
            Ok(report)
        }
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(SelfcheckReport {
            redirection: CheckOutcome::Broken("timed out".to_string()),
            account_swap: CheckOutcome::Broken("timed out".to_string()),
            usage_reading: CheckOutcome::Broken("timed out".to_string()),
            elapsed,
        }),
    }
}

async fn run_checks(settings_path: &Path) -> Result<SelfcheckReport> {
    let settings = PoolSettings::load(settings_path)?;
    settings.validate()?;

    let usage_path = throwaway_usage_path(settings_path);
    let _ = std::fs::remove_file(&usage_path);
    let cleanup = ThrowawayUsageFile(usage_path.clone());

    let account_count = settings.profiles.len();
    // Reuse the first configured pool account's own CODEX_HOME as the scratch
    // driver: safe, since the proxy substitutes whichever account it selects
    // before anything leaves the machine, so the driver's own identity is
    // never what actually pays.
    let driver_home = settings
        .profiles
        .first()
        .map(|profile| profile.home.clone())
        .context("settings file has no configured accounts")?;

    let credentials = settings.load_credentials().await;
    if !credentials.errors.is_empty() {
        return Ok(SelfcheckReport {
            redirection: CheckOutcome::Broken(format!(
                "could not load credentials: {}",
                credentials.errors.join("; ")
            )),
            account_swap: CheckOutcome::Broken("credentials did not load".to_string()),
            usage_reading: CheckOutcome::Broken("credentials did not load".to_string()),
            elapsed: Duration::ZERO,
        });
    }

    let (addr1, handle1) =
        serve_ephemeral(&settings, credentials.loaded, usage_path.clone()).await?;
    let first_call = probe_via_codex_exec(addr1, &driver_home).await;
    let first_payer = current_paying_account(&usage_path);
    handle1.abort();

    let redirection = match &first_call {
        Ok(()) => CheckOutcome::Ok("a real turn completed through the proxy".to_string()),
        Err(reason) => CheckOutcome::Broken(reason.clone()),
    };

    let account_swap = if account_count < 2 {
        CheckOutcome::Skipped("only 1 account configured".to_string())
    } else if redirection.is_broken() {
        CheckOutcome::Broken("skipped: redirection is broken".to_string())
    } else {
        match first_payer {
            None => CheckOutcome::Broken(
                "no account was recorded as paying on the first call".to_string(),
            ),
            Some(first_payer) => {
                force_account_over_threshold(&usage_path, &first_payer)?;
                let credentials = settings.load_credentials().await;
                let (addr2, handle2) =
                    serve_ephemeral(&settings, credentials.loaded, usage_path.clone()).await?;
                let second_call = probe_via_codex_exec(addr2, &driver_home).await;
                let second_payer = current_paying_account(&usage_path);
                handle2.abort();
                match (second_call, second_payer) {
                    (Err(reason), _) => {
                        CheckOutcome::Broken(format!("second call failed: {reason}"))
                    }
                    (Ok(()), None) => CheckOutcome::Broken(
                        "no account was recorded as paying on the second call".to_string(),
                    ),
                    (Ok(()), Some(second_payer)) if second_payer == first_payer => {
                        CheckOutcome::Broken(format!(
                            "both calls paid by '{first_payer}'; expected a different account"
                        ))
                    }
                    (Ok(()), Some(second_payer)) => {
                        CheckOutcome::Ok(format!("{first_payer} -> {second_payer}"))
                    }
                }
            }
        }
    };

    let (addr_for_usage, usage_handle) = {
        let credentials = settings.load_credentials().await;
        serve_ephemeral(&settings, credentials.loaded, usage_path.clone()).await?
    };
    let usage_reading = match reqwest::Client::new()
        .get(format!("http://{addr_for_usage}{USAGE_PATH}"))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => match response.bytes().await {
            Ok(body) => match verify_usage_reading(&body, account_count) {
                Ok(row_count) => CheckOutcome::Ok(format!(
                    "{row_count} rows ({account_count} account(s) + pool total)"
                )),
                Err(reason) => CheckOutcome::Broken(reason),
            },
            Err(error) => CheckOutcome::Broken(format!("could not read response body: {error}")),
        },
        Ok(response) => CheckOutcome::Broken(format!(
            "usage endpoint returned status {}",
            response.status()
        )),
        Err(error) => CheckOutcome::Broken(format!("usage request failed: {error}")),
    };
    usage_handle.abort();

    drop(cleanup);
    Ok(SelfcheckReport {
        redirection,
        account_swap,
        usage_reading,
        elapsed: Duration::ZERO,
    })
}

/// Runs one trivial, real, non-interactive `codex exec` turn through the
/// ephemeral proxy at `addr`, using `driver_home` as its `CODEX_HOME`. Forces
/// the HTTP transport (rather than the newer WebSocket one) so the call is a
/// plain request/response round trip with no session state to manage.
async fn probe_via_codex_exec(
    addr: std::net::SocketAddr,
    driver_home: &Path,
) -> Result<(), String> {
    let codex_binary = resolve_codex_binary();
    let base_url = format!("http://{addr}/backend-api/codex");
    let mut command = tokio::process::Command::new(&codex_binary);
    command
        .env("CODEX_HOME", driver_home)
        .arg("exec")
        .arg("--skip-git-repo-check")
        .arg("-c")
        .arg("model_providers.selfcheck.name=\"selfcheck\"")
        .arg("-c")
        .arg(format!("model_providers.selfcheck.base_url=\"{base_url}\""))
        .arg("-c")
        .arg("model_providers.selfcheck.wire_api=\"responses\"")
        .arg("-c")
        .arg("model_providers.selfcheck.requires_openai_auth=true")
        .arg("-c")
        .arg("model_providers.selfcheck.supports_websockets=false")
        .arg("-c")
        .arg("model_provider=\"selfcheck\"")
        .arg("Reply with the single word: ok")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);

    let output = tokio::time::timeout(PER_CALL_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            format!(
                "'{}' timed out after {PER_CALL_TIMEOUT:?}",
                codex_binary.display()
            )
        })?
        .map_err(|error| format!("could not run '{}': {error}", codex_binary.display()))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "'{}' exited with {}",
            codex_binary.display(),
            output.status
        ))
    }
}

/// Looks for a `codex` binary next to this program first (the common case for
/// a from-source build where both are produced into the same output
/// directory), falling back to whatever `codex` resolves to on `PATH`.
fn resolve_codex_binary() -> PathBuf {
    let exe_name = if cfg!(windows) { "codex.exe" } else { "codex" };
    if let Ok(current_exe) = std::env::current_exe()
        && let Some(dir) = current_exe.parent()
    {
        let sibling = dir.join(exe_name);
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from(if cfg!(windows) { "codex.exe" } else { "codex" })
}

/// Deletes the throwaway usage file on drop, regardless of how `run_checks` exits.
struct ThrowawayUsageFile(PathBuf);

impl Drop for ThrowawayUsageFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn throwaway_usage_path(settings_path: &Path) -> PathBuf {
    let mut path = settings_path.to_path_buf();
    path.set_extension("selfcheck.usage.json");
    path
}

fn current_paying_account(usage_path: &Path) -> Option<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    UsageStore::load(usage_path, now).store.snapshot().1
}

/// Writes a synthetic usage record pushing `label` over any reasonable
/// switch-over threshold, so a second ephemeral instance reading the same
/// file is forced to pick a different account. This is a deliberate,
/// self-contained way to exercise the real account-selection code path
/// without needing a second live account swap to happen organically —
/// `selfcheck` only needs to prove the swapping *mechanism* still works after
/// an upgrade, not re-derive real billing figures (that's MC-37/38's job).
fn force_account_over_threshold(usage_path: &Path, label: &str) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time is before the Unix epoch")?
        .as_secs() as i64;
    let store = UsageStore::load(usage_path, now).store;
    store.record(
        label,
        AccountUsage {
            used_percent: 100.0,
            window_minutes: 300,
            resets_at: now + 3600,
        },
    )
}

#[cfg(test)]
#[path = "selfcheck_tests.rs"]
mod tests;
