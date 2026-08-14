use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_quota_proxy::win_env::PathUpdateOutcome;
use codex_quota_proxy::win_env::binary_install_dir;
use codex_quota_proxy::win_env::update_user_path;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

/// Finds `multi-codex.exe` next to this wizard — both are built together by
/// the same `cargo build --release`, so they always land in the same
/// `target/release/` directory. Mirrors the sibling-lookup pattern
/// `resolve_codex_binary()` already uses elsewhere in this crate.
fn resolve_multi_codex_binary() -> Result<PathBuf> {
    let current_exe =
        std::env::current_exe().context("could not determine this program's own path")?;
    let dir = current_exe
        .parent()
        .context("could not determine this program's directory")?;
    let candidate = dir.join("multi-codex.exe");
    if candidate.is_file() {
        Ok(candidate)
    } else {
        bail!(
            "could not find 'multi-codex.exe' next to this wizard (looked in {}). \
             Build it first with `cargo build -p codex-quota-proxy --release --bin multi-codex`, \
             then re-run this wizard from the same target/release directory.",
            dir.display()
        );
    }
}

/// Looks for another binary (`codex.exe`, `codex-quota-proxy.exe`) next to
/// this wizard. `multi-codex login`/`setup`/the default launch all look for
/// these as siblings of wherever `multi-codex.exe` is currently running
/// from — true today only because everything in this crate lands in the
/// same `target/release/` directory, but no longer true once
/// `multi-codex.exe` is copied to its permanent install location. Bundling
/// each one alongside it there keeps those lookups working, and makes the
/// install self-contained even on a machine with no separately-installed
/// Codex CLI. Returns `None` (not an error) if the named binary isn't next
/// to the wizard — for `codex.exe` specifically, a normal Codex CLI install
/// already puts it on `PATH`, which `multi-codex` falls back to.
fn resolve_bundled_sibling(exe_name: &str) -> Result<Option<PathBuf>> {
    let current_exe =
        std::env::current_exe().context("could not determine this program's own path")?;
    let dir = current_exe
        .parent()
        .context("could not determine this program's directory")?;
    let candidate = dir.join(exe_name);
    Ok(if candidate.is_file() {
        Some(candidate)
    } else {
        None
    })
}

async fn offer_to_add_account(installed_binary: &Path) -> Result<()> {
    print!("\nWould you like to log into an account now? [y/N]: ");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("could not read your answer")?;
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Skipping. Run `multi-codex setup` any time to add accounts.");
        return Ok(());
    }

    let status = tokio::process::Command::new(installed_binary)
        .arg("setup")
        .status()
        .await
        .with_context(|| format!("could not run '{}'", installed_binary.display()))?;
    if !status.success() {
        bail!("multi-codex setup exited with {status}");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    if !cfg!(windows) {
        bail!(
            "this installer is Windows-only; on macOS/Linux, follow the manual PATH \
             instructions in the README instead."
        );
    }

    println!("multi-codex install wizard");
    println!("===========================\n");

    let multi_codex_binary = resolve_multi_codex_binary()?;
    println!("Found multi-codex at {}", multi_codex_binary.display());

    let install_dir = binary_install_dir()?;
    std::fs::create_dir_all(&install_dir)
        .with_context(|| format!("could not create install directory {}", install_dir.display()))?;
    let install_target = install_dir.join("multi-codex.exe");
    std::fs::copy(&multi_codex_binary, &install_target).with_context(|| {
        format!(
            "could not copy multi-codex to {}",
            install_target.display()
        )
    })?;
    println!("Installed multi-codex to {}", install_target.display());

    match resolve_bundled_sibling("codex.exe")? {
        Some(codex_binary) => {
            let codex_target = install_dir.join("codex.exe");
            std::fs::copy(&codex_binary, &codex_target).with_context(|| {
                format!("could not copy codex to {}", codex_target.display())
            })?;
            println!("Also bundled codex from {}", codex_binary.display());
        }
        None => {
            println!(
                "No codex.exe found next to this wizard — assuming the official Codex CLI is \
                 already installed and on PATH."
            );
        }
    }

    match resolve_bundled_sibling("codex-quota-proxy.exe")? {
        Some(proxy_binary) => {
            let proxy_target = install_dir.join("codex-quota-proxy.exe");
            std::fs::copy(&proxy_binary, &proxy_target).with_context(|| {
                format!("could not copy codex-quota-proxy to {}", proxy_target.display())
            })?;
            println!("Also bundled codex-quota-proxy from {}", proxy_binary.display());
        }
        None => {
            println!(
                "Warning: no codex-quota-proxy.exe found next to this wizard. Plain `multi-codex` \
                 (with no subcommand) needs it to actually start the pool and will fail without \
                 it — rebuild with `--bin codex-quota-proxy` included and re-run this wizard. \
                 `login`/`setup`/`accounts`/etc. don't need it and will still work."
            );
        }
    }

    let path_var_name =
        std::env::var("MULTI_CODEX_WIZARD_TEST_ENV_VAR").unwrap_or_else(|_| "PATH".to_string());
    match update_user_path(&path_var_name, &install_dir).await? {
        PathUpdateOutcome::AlreadyPresent => {
            println!("'{}' is already on your {path_var_name}.", install_dir.display());
        }
        PathUpdateOutcome::Added => {
            println!(
                "Added '{}' to your user {path_var_name}. Open a new terminal for it to take effect.",
                install_dir.display()
            );
        }
    }

    offer_to_add_account(&install_target).await?;

    println!("\nAll done. Open a new terminal and run `multi-codex`.");
    Ok(())
}
