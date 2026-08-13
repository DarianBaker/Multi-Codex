use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_login::token_data::parse_jwt_expiration;
use codex_quota_proxy::PoolSettings;
use codex_quota_proxy::pool_status;
use codex_quota_proxy::resolve_codex_binary;
use codex_quota_proxy::run_selfcheck;
use codex_quota_proxy::serve;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

/// Resolves `~/.multi-codex`. Honors `MULTI_CODEX_HOME` first so tests and
/// manual verification runs can redirect this to a scratch directory —
/// `dirs::home_dir()` reads platform APIs directly (`SHGetKnownFolderPath` on
/// Windows) and does **not** consult `HOME`/`USERPROFILE`, so there is no
/// other way to isolate a run on this platform. This mirrors
/// `codex-utils-home-dir`'s `CODEX_HOME` override for the same reason.
fn multi_codex_home() -> Result<PathBuf> {
    if let Ok(override_home) = std::env::var("MULTI_CODEX_HOME") {
        return Ok(PathBuf::from(override_home));
    }
    let home = dirs::home_dir().context("could not find your home directory")?;
    Ok(home.join(".multi-codex"))
}

fn pool_toml_path() -> Result<PathBuf> {
    Ok(multi_codex_home()?.join("pool.toml"))
}

fn usage_path_for(pool_toml: &Path) -> PathBuf {
    let mut path = pool_toml.to_path_buf();
    path.set_extension("usage.json");
    path
}

fn accounts_dir() -> Result<PathBuf> {
    Ok(multi_codex_home()?.join("accounts"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    match args.next() {
        None => launch().await,
        Some(sub) if sub == "login" => {
            let label = args
                .next()
                .context("usage: multi-codex login <label> [--main]")?
                .into_string()
                .map_err(|_| anyhow::anyhow!("account label must be valid UTF-8"))?;
            let is_main = args.next().is_some_and(|arg| arg == "--main");
            login(&label, is_main).await
        }
        Some(sub) if sub == "setup" => setup().await,
        Some(sub) if sub == "accounts" => accounts(),
        Some(sub) if sub == "check" => check().await,
        Some(sub) if sub == "status" => status(),
        Some(sub) if sub == "selfcheck" => selfcheck().await,
        Some(other) => bail!("unknown subcommand '{}'", other.to_string_lossy()),
    }
}

fn accounts() -> Result<()> {
    let pool_toml = pool_toml_path()?;
    PoolSettings::ensure_exists(&pool_toml)?;
    let settings = PoolSettings::load(&pool_toml)?;

    let mut profiles: Vec<_> = settings.profiles.iter().collect();
    profiles.sort_by_key(|profile| profile.priority);
    if profiles.is_empty() {
        println!("no accounts configured yet; run `multi-codex login <label>`");
    }
    for profile in profiles {
        let marker = if profile.is_main { " (main)" } else { "" };
        println!(
            "'{}': priority={}{marker}",
            profile.label, profile.priority
        );
    }
    Ok(())
}

async fn check() -> Result<()> {
    let pool_toml = pool_toml_path()?;
    PoolSettings::ensure_exists(&pool_toml)?;
    let settings = PoolSettings::load(&pool_toml)?;
    settings.validate()?;
    let report = settings.load_credentials().await;
    let mut broken = report.errors.len();

    // Mirrors `codex-quota-proxy check`'s per-account diagnostic detail
    // (main.rs's `check`) rather than a thinner reimplementation, so users
    // get the same specific reasons (missing tokens/email/plan, unreadable
    // or expired JWT) regardless of which binary they run.
    for account in report.loaded {
        let Some(tokens) = account.credentials.tokens.as_ref() else {
            println!(
                "BROKEN '{}': login tokens are missing; run `multi-codex login {}`",
                account.label, account.label
            );
            broken += 1;
            continue;
        };
        let Some(email) = tokens.id_token.email.as_deref() else {
            println!(
                "BROKEN '{}': login email is missing; run `multi-codex login {}`",
                account.label, account.label
            );
            broken += 1;
            continue;
        };
        let Some(plan) = tokens.id_token.get_chatgpt_plan_type() else {
            println!(
                "BROKEN '{}': account plan is missing; run `multi-codex login {}`",
                account.label, account.label
            );
            broken += 1;
            continue;
        };
        let expires_at = match parse_jwt_expiration(&tokens.access_token) {
            Ok(Some(expires_at)) => expires_at,
            Ok(None) => {
                println!(
                    "BROKEN '{}': login expiry is missing; run `multi-codex login {}`",
                    account.label, account.label
                );
                broken += 1;
                continue;
            }
            Err(error) => {
                println!(
                    "BROKEN '{}': login expiry is unreadable: {error}; run `multi-codex login {}`",
                    account.label, account.label
                );
                broken += 1;
                continue;
            }
        };

        println!(
            "READY '{}': email={email} plan={plan} expires={}",
            account.label,
            expires_at.to_rfc3339()
        );
    }

    for error in report.errors {
        println!("BROKEN {error}");
    }
    if broken > 0 {
        bail!("{broken} account(s) are broken");
    }
    Ok(())
}

fn status() -> Result<()> {
    let pool_toml = pool_toml_path()?;
    PoolSettings::ensure_exists(&pool_toml)?;
    let settings = PoolSettings::load(&pool_toml)?;
    let usage_path = usage_path_for(&pool_toml);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system time is before the Unix epoch")?
        .as_secs() as i64;
    let status = pool_status(&settings, usage_path, now);
    if let Some(warning) = status.warning {
        eprintln!("{warning}");
    }
    print!("{}", status.output);
    Ok(())
}

async fn selfcheck() -> Result<()> {
    let pool_toml = pool_toml_path()?;
    PoolSettings::ensure_exists(&pool_toml)?;
    let report = run_selfcheck(&pool_toml).await?;
    print!("{}", report.render());
    if report.passed() {
        Ok(())
    } else {
        bail!("selfcheck found a broken component");
    }
}

async fn login(label: &str, is_main: bool) -> Result<()> {
    codex_quota_proxy::validate_label(label)?;

    let account_home = accounts_dir()?.join(label);
    std::fs::create_dir_all(&account_home)
        .with_context(|| format!("could not create account directory {}", account_home.display()))?;

    let codex_binary = resolve_codex_binary();
    let status = tokio::process::Command::new(&codex_binary)
        .env("CODEX_HOME", &account_home)
        .arg("login")
        .status()
        .await
        .with_context(|| {
            format!(
                "could not run '{}' — is codex installed and on PATH?",
                codex_binary.display()
            )
        })?;

    if !status.success() {
        bail!(
            "`codex login` for account '{label}' did not complete successfully (exit status {status})"
        );
    }

    let pool_toml = pool_toml_path()?;
    PoolSettings::ensure_exists(&pool_toml)?;
    let mut settings = PoolSettings::load(&pool_toml)?;
    settings.upsert_profile(label, account_home, is_main)?;
    settings.save(&pool_toml)?;

    println!("account '{label}' saved to {}", pool_toml.display());
    Ok(())
}

async fn setup() -> Result<()> {
    todo!("Task 10")
}

async fn launch() -> Result<()> {
    todo!("Task 11")
}
