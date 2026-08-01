use anyhow::Result;
use anyhow::bail;
use codex_login::token_data::parse_jwt_expiration;
use codex_quota_proxy::PoolSettings;
use codex_quota_proxy::serve;
use std::ffi::OsString;

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

        let path = first;
        let settings = PoolSettings::load(path)?;
        settings.validate()?;
        let report = settings.load_credentials().await;

        for account in report.loaded {
            println!("account '{}': {}", account.label, account.identifier);
        }
        for error in &report.errors {
            eprintln!("{error}");
        }
        if !report.errors.is_empty() {
            bail!("one or more accounts could not load credentials");
        }

        return serve(&settings.listen_addr, &settings.upstream_base).await;
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
