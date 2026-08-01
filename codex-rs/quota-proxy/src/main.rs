use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_login::token_data::parse_jwt_expiration;
use codex_quota_proxy::PoolSettings;
use codex_quota_proxy::pool_status;
use codex_quota_proxy::serve;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    if let Some(first) = args.next() {
        if first == "check" {
            let path = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("usage: codex-quota-proxy check <settings-file>"))?;
            return check(path).await;
        }
        if first == "status" {
            let path = args.next().ok_or_else(|| {
                anyhow::anyhow!("usage: codex-quota-proxy status <settings-file>")
            })?;
            return status(path);
        }

        let path = first;
        let settings = PoolSettings::load(&path)?;
        settings.validate()?;
        let report = settings.load_credentials().await;

        for account in &report.loaded {
            println!("account '{}': {}", account.label, account.identifier);
        }
        for error in &report.errors {
            eprintln!("{error}");
        }

        // Never use the main account as an automatic fallback.
        let account = report
            .loaded
            .into_iter()
            .find(|account| {
                settings
                    .profiles
                    .iter()
                    .any(|profile| profile.label == account.label && !profile.is_main)
            })
            .context("no usable secondary account; main account will not be used")?;
        let mut usage_path = PathBuf::from(path);
        usage_path.set_extension("usage.json");
        return serve(
            &settings.listen_addr,
            &settings.upstream_base,
            account,
            usage_path,
        )
        .await;
    }

    // Show which build started before later proxy work adds long-running behavior.
    println!("codex-quota-proxy {}", env!("CARGO_PKG_VERSION"));
    Ok(())
}

async fn check(path: OsString) -> Result<()> {
    let settings = PoolSettings::load(path)?;
    settings.validate()?;
    let report = settings.load_credentials().await;
    let mut broken = report.errors.len();

    // Print every account before returning a failing exit code.
    for account in report.loaded {
        let Some(tokens) = account.credentials.tokens.as_ref() else {
            println!(
                "BROKEN '{}': login tokens are missing; run `codex login`",
                account.label
            );
            broken += 1;
            continue;
        };
        let Some(email) = tokens.id_token.email.as_deref() else {
            println!(
                "BROKEN '{}': login email is missing; run `codex login`",
                account.label
            );
            broken += 1;
            continue;
        };
        let Some(plan) = tokens.id_token.get_chatgpt_plan_type() else {
            println!(
                "BROKEN '{}': account plan is missing; run `codex login`",
                account.label
            );
            broken += 1;
            continue;
        };
        let expires_at = match parse_jwt_expiration(&tokens.access_token) {
            Ok(Some(expires_at)) => expires_at,
            Ok(None) => {
                println!(
                    "BROKEN '{}': login expiry is missing; run `codex login`",
                    account.label
                );
                broken += 1;
                continue;
            }
            Err(error) => {
                println!(
                    "BROKEN '{}': login expiry is unreadable: {error}; run `codex login`",
                    account.label
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

fn status(path: OsString) -> Result<()> {
    let settings = PoolSettings::load(&path)?;
    let mut usage_path = PathBuf::from(path);
    usage_path.set_extension("usage.json");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time is before the Unix epoch")?
        .as_secs() as i64;
    let status = pool_status(&settings, usage_path, now);
    if let Some(warning) = status.warning {
        eprintln!("{warning}");
    }
    print!("{}", status.output);
    Ok(())
}
