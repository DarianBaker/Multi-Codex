use anyhow::Result;
use anyhow::bail;
use codex_quota_proxy::PoolSettings;

#[tokio::main]
async fn main() -> Result<()> {
    if let Some(path) = std::env::args_os().nth(1) {
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
    }

    // Show which build started before later proxy work adds long-running behavior.
    println!("codex-quota-proxy {}", env!("CARGO_PKG_VERSION"));
    Ok(())
}
