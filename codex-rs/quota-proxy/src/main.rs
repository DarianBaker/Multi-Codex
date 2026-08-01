use anyhow::Result;
use codex_quota_proxy::PoolSettings;

fn main() -> Result<()> {
    if let Some(path) = std::env::args_os().nth(1) {
        PoolSettings::load(path)?.validate()?;
    }

    // Show which build started before later proxy work adds long-running behavior.
    println!("codex-quota-proxy {}", env!("CARGO_PKG_VERSION"));
    Ok(())
}
