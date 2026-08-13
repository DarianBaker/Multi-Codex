use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

/// Returns true if `dir` (case-insensitively, ignoring a trailing slash on
/// either side) already appears as one of the `;`-separated entries in
/// `path_value`.
fn path_contains_dir(path_value: &str, dir: &str) -> bool {
    let dir = normalize(dir);
    path_value
        .split(';')
        .any(|entry| normalize(entry) == dir)
}

fn normalize(dir: &str) -> String {
    dir.trim()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

/// Appends `dir` to `path_value` as a new `;`-separated entry, unless it's
/// already present. Returns `None` if no change is needed, so the caller can
/// tell "already set up" apart from "just changed it".
fn append_dir_if_missing(path_value: &str, dir: &str) -> Option<String> {
    if path_contains_dir(path_value, dir) {
        return None;
    }
    if path_value.trim().is_empty() {
        Some(dir.to_string())
    } else if path_value.trim_end().ends_with(';') {
        Some(format!("{path_value}{dir}"))
    } else {
        Some(format!("{path_value};{dir}"))
    }
}

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

/// Where `multi-codex.exe` gets installed to. `MULTI_CODEX_WIZARD_INSTALL_DIR`
/// lets tests and manual verification redirect this to a scratch directory
/// instead of the real per-user install location, same convention as
/// `MULTI_CODEX_HOME` in `multi_codex.rs`.
fn install_dir() -> Result<PathBuf> {
    if let Ok(override_dir) = std::env::var("MULTI_CODEX_WIZARD_INSTALL_DIR") {
        return Ok(PathBuf::from(override_dir));
    }
    let local_app_data =
        dirs::data_local_dir().context("could not find your local app data directory")?;
    Ok(local_app_data.join("multi-codex").join("bin"))
}

/// Reads a **user-level** Windows environment variable via PowerShell. The
/// variable name is passed through the child's own environment (`MCW_VAR_NAME`)
/// rather than interpolated into the command text, so a variable name or
/// value containing quotes can never be misparsed as PowerShell syntax.
async fn read_user_env_var(name: &str) -> Result<String> {
    let output = tokio::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::GetEnvironmentVariable($env:MCW_VAR_NAME, 'User')",
        ])
        .env("MCW_VAR_NAME", name)
        .output()
        .await
        .context("could not run powershell to read the current environment variable")?;
    if !output.status.success() {
        bail!(
            "powershell exited with {} while reading '{name}'",
            output.status
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_string())
}

/// Writes a **user-level** Windows environment variable via PowerShell.
/// `[Environment]::SetEnvironmentVariable` (unlike `setx`) has no 1024-character
/// truncation limit and broadcasts the change so most already-running
/// programs notice it without a reboot (open terminals still need reopening).
/// Same safe-quoting approach as `read_user_env_var`.
async fn write_user_env_var(name: &str, value: &str) -> Result<()> {
    let status = tokio::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::SetEnvironmentVariable($env:MCW_VAR_NAME, $env:MCW_VAR_VALUE, 'User')",
        ])
        .env("MCW_VAR_NAME", name)
        .env("MCW_VAR_VALUE", value)
        .status()
        .await
        .context("could not run powershell to update the environment variable")?;
    if !status.success() {
        bail!(
            "powershell exited with {status} while updating '{name}'"
        );
    }
    Ok(())
}

async fn update_user_path(var_name: &str, install_dir: &Path) -> Result<()> {
    let current = read_user_env_var(var_name).await?;
    let install_dir_str = install_dir.to_string_lossy();
    match append_dir_if_missing(&current, &install_dir_str) {
        None => {
            println!("'{}' is already on your {var_name}.", install_dir.display());
        }
        Some(updated) => {
            write_user_env_var(var_name, &updated).await?;
            println!(
                "Added '{}' to your user {var_name}. Open a new terminal for it to take effect.",
                install_dir.display()
            );
        }
    }
    Ok(())
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
        .with_context(|| {
            format!(
                "could not run '{}'",
                installed_binary.display()
            )
        })?;
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

    let install_dir = install_dir()?;
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

    let path_var_name =
        std::env::var("MULTI_CODEX_WIZARD_TEST_ENV_VAR").unwrap_or_else(|_| "PATH".to_string());
    update_user_path(&path_var_name, &install_dir).await?;

    offer_to_add_account(&install_target).await?;

    println!("\nAll done. Open a new terminal and run `multi-codex`.");
    Ok(())
}

// Tests live inline (rather than in a separate `src/bin/wizard_tests.rs`
// file, the convention used elsewhere in this crate) because cargo's
// `autobins` auto-discovery treats every loose `.rs` file directly under
// `src/bin/` as its own binary target — a sibling test file there breaks
// the build with "consider adding a `main` function".
#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::append_dir_if_missing;
    use super::path_contains_dir;

    #[test]
    fn path_contains_dir_finds_exact_match() {
        assert!(path_contains_dir(
            r"C:\a;C:\Users\me\.multi-codex\bin;C:\b",
            r"C:\Users\me\.multi-codex\bin"
        ));
    }

    #[test]
    fn path_contains_dir_is_case_insensitive() {
        assert!(path_contains_dir(
            r"C:\A;C:\USERS\ME\.MULTI-CODEX\BIN",
            r"C:\Users\me\.multi-codex\bin"
        ));
    }

    #[test]
    fn path_contains_dir_ignores_a_trailing_slash_either_side() {
        assert!(path_contains_dir(
            r"C:\a;C:\Users\me\.multi-codex\bin\",
            r"C:\Users\me\.multi-codex\bin"
        ));
        assert!(path_contains_dir(
            r"C:\a;C:\Users\me\.multi-codex\bin",
            r"C:\Users\me\.multi-codex\bin\"
        ));
    }

    #[test]
    fn path_contains_dir_is_false_when_absent() {
        assert!(!path_contains_dir(
            r"C:\a;C:\b",
            r"C:\Users\me\.multi-codex\bin"
        ));
    }

    #[test]
    fn path_contains_dir_handles_empty_path() {
        assert!(!path_contains_dir("", r"C:\Users\me\.multi-codex\bin"));
    }

    #[test]
    fn append_dir_if_missing_appends_to_a_populated_path() {
        let result = append_dir_if_missing(r"C:\a;C:\b", r"C:\Users\me\.multi-codex\bin");
        assert_eq!(
            result,
            Some(r"C:\a;C:\b;C:\Users\me\.multi-codex\bin".to_string())
        );
    }

    #[test]
    fn append_dir_if_missing_handles_a_trailing_semicolon_without_doubling_it() {
        let result = append_dir_if_missing(r"C:\a;C:\b;", r"C:\Users\me\.multi-codex\bin");
        assert_eq!(
            result,
            Some(r"C:\a;C:\b;C:\Users\me\.multi-codex\bin".to_string())
        );
    }

    #[test]
    fn append_dir_if_missing_handles_an_empty_existing_path() {
        let result = append_dir_if_missing("", r"C:\Users\me\.multi-codex\bin");
        assert_eq!(result, Some(r"C:\Users\me\.multi-codex\bin".to_string()));
    }

    #[test]
    fn append_dir_if_missing_returns_none_when_already_present() {
        let result = append_dir_if_missing(
            r"C:\a;C:\Users\me\.multi-codex\bin;C:\b",
            r"C:\Users\me\.multi-codex\bin",
        );
        assert_eq!(result, None);
    }

    #[test]
    fn append_dir_if_missing_returns_none_when_already_present_case_insensitively() {
        let result = append_dir_if_missing(
            r"C:\a;C:\USERS\ME\.MULTI-CODEX\BIN;C:\b",
            r"C:\Users\me\.multi-codex\bin",
        );
        assert_eq!(result, None);
    }
}
