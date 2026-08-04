# `multi-codex` launcher Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a new `multi-codex` binary (same crate as `codex-quota-proxy`) that manages account credentials in `~/.multi-codex/` and launches Codex already wired to the account pool, so a user never hand-writes a settings file or starts the proxy separately.

**Architecture:** Extend `codex-rs/quota-proxy/src/settings.rs` with a `Serialize` impl and three small, independently-testable functions (`save`, `ensure_exists`, `validate_label`, `upsert_profile`) that the new binary calls. Everything else — path resolution, subcommand dispatch, shelling out to `codex login`, and spawning the real interactive `codex` child — lives in one new file, `src/bin/multi_codex.rs`, and is proven by running it for real rather than by unit test, per the approved design doc.

**Tech Stack:** Rust, existing `codex-quota-proxy` crate (axum, tokio, toml, reqwest already present); adds one new workspace-already-available dependency (`dirs`).

**Design doc:** `docs/superpowers/specs/2026-08-02-multi-codex-launcher-design.md` — read this first for the *why* behind every decision below (priority recompute algorithm, label restrictions, fail-closed vs fail-open, the interactive-spawn risk). This plan only covers the *how*.

---

## Chunk 1: `settings.rs` — writable, auto-creating pool settings

### Task 1: Add `Serialize` to `PoolSettings` and `ProfileSettings`

**Files:**
- Modify: `codex-rs/quota-proxy/src/settings.rs:1-41`
- Test: `codex-rs/quota-proxy/src/settings_tests.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `codex-rs/quota-proxy/src/settings_tests.rs`:

```rust
use pretty_assertions::assert_eq;

use super::PoolSettings;
use super::ProfileSettings;

#[test]
fn serialize_then_deserialize_round_trips() {
    let settings = PoolSettings {
        listen_addr: "127.0.0.1:8788".to_string(),
        upstream_base: "https://chatgpt.com/backend-api/codex".to_string(),
        default_switch_at_percent: 80.0,
        profiles: vec![ProfileSettings {
            label: "work".to_string(),
            home: "/tmp/work".into(),
            priority: 0,
            switch_at_percent: None,
            is_main: false,
        }],
    };

    let toml_text = toml::to_string_pretty(&settings).expect("serialize settings");
    let reloaded: PoolSettings = toml::from_str(&toml_text).expect("deserialize settings");

    assert_eq!(reloaded.listen_addr, settings.listen_addr);
    assert_eq!(reloaded.default_switch_at_percent, settings.default_switch_at_percent);
    assert_eq!(reloaded.profiles.len(), 1);
    assert_eq!(reloaded.profiles[0].label, "work");
    assert_eq!(reloaded.profiles[0].priority, 0);
}
```

Wire it in at the bottom of `settings.rs`:

```rust
#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p codex-quota-proxy serialize_then_deserialize_round_trips`
Expected: FAIL to compile — `the trait bound PoolSettings: Serialize is not satisfied` (or similar), since only `Deserialize` is derived today.

- [ ] **Step 3: Add the derive**

In `codex-rs/quota-proxy/src/settings.rs`, change:

```rust
use serde::Deserialize;
```
to
```rust
use serde::Deserialize;
use serde::Serialize;
```

and change both derives:

```rust
#[derive(Debug, Deserialize, Serialize)]
pub struct PoolSettings {
```
```rust
#[derive(Debug, Deserialize, Serialize)]
pub struct ProfileSettings {
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p codex-quota-proxy serialize_then_deserialize_round_trips`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add codex-rs/quota-proxy/src/settings.rs codex-rs/quota-proxy/src/settings_tests.rs
git commit -m "feat(multi-codex): make PoolSettings serializable"
```

---

### Task 2: `PoolSettings::save` — write settings back to disk

**Files:**
- Modify: `codex-rs/quota-proxy/src/settings.rs`
- Test: `codex-rs/quota-proxy/src/settings_tests.rs`

- [ ] **Step 1: Write the failing test**

Append to `settings_tests.rs`:

```rust
#[test]
fn save_then_load_round_trips_through_a_real_file() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let path = temp.path().join("pool.toml");

    let settings = PoolSettings {
        listen_addr: "127.0.0.1:8788".to_string(),
        upstream_base: "https://chatgpt.com/backend-api/codex".to_string(),
        default_switch_at_percent: 80.0,
        profiles: vec![ProfileSettings {
            label: "work".to_string(),
            home: temp.path().join("work"),
            priority: 0,
            switch_at_percent: None,
            is_main: false,
        }],
    };

    settings.save(&path).expect("save settings");
    let reloaded = PoolSettings::load(&path).expect("reload saved settings");

    assert_eq!(reloaded.profiles.len(), 1);
    assert_eq!(reloaded.profiles[0].label, "work");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p codex-quota-proxy save_then_load_round_trips_through_a_real_file`
Expected: FAIL to compile — `no method named save found for struct PoolSettings`.

- [ ] **Step 3: Implement `save`**

In `codex-rs/quota-proxy/src/settings.rs`, inside `impl PoolSettings`, add after `load`:

```rust
/// Writes these settings back to `path` as TOML, overwriting whatever is there.
pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let text = toml::to_string_pretty(self).context("could not serialize settings")?;
    fs::write(path, text)
        .with_context(|| format!("could not write settings file {}", path.display()))?;
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p codex-quota-proxy --lib settings`
Expected: PASS (both Task 1 and Task 2 tests)

- [ ] **Step 5: Commit**

```bash
git add codex-rs/quota-proxy/src/settings.rs codex-rs/quota-proxy/src/settings_tests.rs
git commit -m "feat(multi-codex): add PoolSettings::save"
```

---

### Task 3: `PoolSettings::ensure_exists` — auto-create a starter file

**Files:**
- Modify: `codex-rs/quota-proxy/src/settings.rs`
- Test: `codex-rs/quota-proxy/src/settings_tests.rs`

Per the design doc, a missing `pool.toml` must be created with an **explicit empty `profile = []`** and `default_switch_at_percent = 80.0` — `PoolSettings::load` requires both fields, so a file with no profile blocks at all does not parse as an empty array on its own; it must be written literally.

- [ ] **Step 1: Write the failing tests**

Append to `settings_tests.rs`:

```rust
#[test]
fn ensure_exists_creates_a_starter_file_when_missing() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let path = temp.path().join("pool.toml");

    PoolSettings::ensure_exists(&path).expect("create starter pool.toml");

    let loaded = PoolSettings::load(&path).expect("load the created starter file");
    assert_eq!(loaded.default_switch_at_percent, 80.0);
    assert_eq!(loaded.profiles.len(), 0);
}

#[test]
fn ensure_exists_does_not_touch_an_existing_file() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let path = temp.path().join("pool.toml");
    std::fs::write(&path, "default_switch_at_percent = 55.0\nprofile = []\n")
        .expect("write existing file");

    PoolSettings::ensure_exists(&path).expect("ensure_exists on an existing file");

    let loaded = PoolSettings::load(&path).expect("load the untouched file");
    assert_eq!(loaded.default_switch_at_percent, 55.0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p codex-quota-proxy ensure_exists`
Expected: FAIL to compile — `no function or associated item named ensure_exists found`.

- [ ] **Step 3: Implement `ensure_exists`**

In `codex-rs/quota-proxy/src/settings.rs`, inside `impl PoolSettings`, add:

```rust
/// Creates a starter settings file at `path` if nothing exists there yet.
/// Leaves an existing file untouched. The written file has the explicit
/// empty `profile = []` array `PoolSettings::load` requires, not merely an
/// absent field.
pub fn ensure_exists(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create directory {}", parent.display()))?;
    }
    let starter = PoolSettings {
        listen_addr: default_listen_addr(),
        upstream_base: default_upstream_base(),
        default_switch_at_percent: 80.0,
        profiles: Vec::new(),
    };
    starter.save(path)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p codex-quota-proxy --lib settings`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add codex-rs/quota-proxy/src/settings.rs codex-rs/quota-proxy/src/settings_tests.rs
git commit -m "feat(multi-codex): add PoolSettings::ensure_exists"
```

---

### Task 4: `validate_label` — restrict account labels

**Files:**
- Modify: `codex-rs/quota-proxy/src/settings.rs`
- Test: `codex-rs/quota-proxy/src/settings_tests.rs`

Label is used as a directory name (`~/.multi-codex/accounts/<label>/`), so it is restricted to `[A-Za-z0-9_-]`, rejecting empty strings and the reserved Windows device names (case-insensitive): `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`.

- [ ] **Step 1: Write the failing tests**

Append to `settings_tests.rs`:

```rust
use super::validate_label;

#[test]
fn validate_label_accepts_simple_names() {
    assert!(validate_label("work").is_ok());
    assert!(validate_label("account-2").is_ok());
    assert!(validate_label("my_account").is_ok());
}

#[test]
fn validate_label_rejects_empty_and_bad_characters() {
    assert!(validate_label("").is_err());
    assert!(validate_label("work/account").is_err());
    assert!(validate_label("work account").is_err());
    assert!(validate_label("work.account").is_err());
}

#[test]
fn validate_label_rejects_reserved_windows_device_names_case_insensitively() {
    assert!(validate_label("CON").is_err());
    assert!(validate_label("con").is_err());
    assert!(validate_label("Nul").is_err());
    assert!(validate_label("COM1").is_err());
    assert!(validate_label("lpt9").is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p codex-quota-proxy validate_label`
Expected: FAIL to compile — `unresolved import super::validate_label`.

- [ ] **Step 3: Implement `validate_label`**

In `codex-rs/quota-proxy/src/settings.rs`, add near the top-level functions (after `default_upstream_base`):

```rust
const RESERVED_WINDOWS_DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Rejects labels that are unsafe as a directory name: empty, containing
/// anything outside `[A-Za-z0-9_-]`, or a reserved Windows device name
/// (checked case-insensitively so it is also rejected on non-Windows, keeping
/// pool.toml portable between the two).
pub fn validate_label(label: &str) -> Result<()> {
    if label.is_empty() {
        bail!("account label cannot be empty");
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!(
            "account label '{label}' may only contain letters, digits, '_' and '-'"
        );
    }
    if RESERVED_WINDOWS_DEVICE_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(label))
    {
        bail!("account label '{label}' is a reserved device name and cannot be used");
    }
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p codex-quota-proxy --lib settings`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add codex-rs/quota-proxy/src/settings.rs codex-rs/quota-proxy/src/settings_tests.rs
git commit -m "feat(multi-codex): add validate_label for account directory names"
```

---

### Task 5: `PoolSettings::upsert_profile` — add/update an account with full priority recompute

**Files:**
- Modify: `codex-rs/quota-proxy/src/settings.rs`
- Test: `codex-rs/quota-proxy/src/settings_tests.rs`

Behavior, per the design doc:
- Label matching an existing profile **exactly** → update that profile's `home`/`is_main` in place, keep its position.
- Label matching an existing profile **case-insensitively but not exactly** → error (case-only collision).
- Otherwise → append a new profile.
- If the upserted profile is `is_main: true`, clear `is_main` on every other profile first (only one main can exist; otherwise recompute below cannot assign a single unique highest priority).
- **Recompute every profile's priority from scratch**, in file order: non-main profiles get `0..N-1` in the order they appear; the main profile (if any) gets `N`.

- [ ] **Step 1: Write the failing tests**

Append to `settings_tests.rs`:

```rust
use std::path::PathBuf;

use super::default_listen_addr;
use super::default_upstream_base;

fn empty_pool() -> PoolSettings {
    PoolSettings {
        listen_addr: default_listen_addr(),
        upstream_base: default_upstream_base(),
        default_switch_at_percent: 80.0,
        profiles: Vec::new(),
    }
}

/// Creates a real directory under `temp` for `label` — needed by any test
/// that calls `PoolSettings::validate()`, since it bails if a profile's
/// `home` doesn't exist on disk.
fn account_dir(temp: &tempfile::TempDir, label: &str) -> PathBuf {
    let dir = temp.path().join(label);
    std::fs::create_dir_all(&dir).expect("create account directory");
    dir
}

#[test]
fn upsert_adds_a_non_main_profile_at_priority_zero() {
    let mut pool = empty_pool();
    pool.upsert_profile("work", PathBuf::from("/tmp/work"), false)
        .expect("add work");

    assert_eq!(pool.profiles.len(), 1);
    assert_eq!(pool.profiles[0].label, "work");
    assert_eq!(pool.profiles[0].priority, 0);
    assert!(!pool.profiles[0].is_main);
}

#[test]
fn upsert_main_after_non_main_gets_the_highest_priority() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let mut pool = empty_pool();
    pool.upsert_profile("work", account_dir(&temp, "work"), false)
        .expect("add work");
    pool.upsert_profile("daily", account_dir(&temp, "daily"), true)
        .expect("add daily as main");

    let work = pool.profiles.iter().find(|p| p.label == "work").expect("work");
    let daily = pool.profiles.iter().find(|p| p.label == "daily").expect("daily");
    assert_eq!(work.priority, 0);
    assert_eq!(daily.priority, 1);
    assert!(daily.is_main);
    pool.validate().expect("recomputed priorities must satisfy validate()");
}

#[test]
fn upsert_main_before_non_main_is_recomputed_so_main_stays_highest() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let mut pool = empty_pool();
    pool.upsert_profile("daily", account_dir(&temp, "daily"), true)
        .expect("add daily as main first");
    pool.upsert_profile("work", account_dir(&temp, "work"), false)
        .expect("add work after main already exists");

    let work = pool.profiles.iter().find(|p| p.label == "work").expect("work");
    let daily = pool.profiles.iter().find(|p| p.label == "daily").expect("daily");
    assert_eq!(work.priority, 0);
    assert_eq!(daily.priority, 1, "main must be recomputed to stay the highest priority");
    pool.validate().expect("recomputed priorities must satisfy validate()");
}

#[test]
fn upsert_existing_label_updates_home_in_place_instead_of_duplicating() {
    let mut pool = empty_pool();
    pool.upsert_profile("work", PathBuf::from("/tmp/work-old"), false)
        .expect("add work");
    pool.upsert_profile("work", PathBuf::from("/tmp/work-new"), false)
        .expect("re-add work with a new home");

    assert_eq!(pool.profiles.len(), 1);
    assert_eq!(pool.profiles[0].home, PathBuf::from("/tmp/work-new"));
}

#[test]
fn upsert_rejects_case_only_label_collision() {
    let mut pool = empty_pool();
    pool.upsert_profile("Work", PathBuf::from("/tmp/work"), false)
        .expect("add Work");

    let error = pool
        .upsert_profile("work", PathBuf::from("/tmp/other"), false)
        .expect_err("case-only collision must be rejected");
    assert!(error.to_string().contains("work"));
}

#[test]
fn upsert_rejects_invalid_label() {
    let mut pool = empty_pool();
    let error = pool
        .upsert_profile("bad label", PathBuf::from("/tmp/bad"), false)
        .expect_err("invalid label must be rejected");
    assert!(error.to_string().contains("letters, digits"));
}

#[test]
fn upsert_new_main_demotes_the_previous_main() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let mut pool = empty_pool();
    pool.upsert_profile("old-daily", account_dir(&temp, "old-daily"), true)
        .expect("add old-daily as main");
    pool.upsert_profile("new-daily", account_dir(&temp, "new-daily"), true)
        .expect("add new-daily as main");

    let old = pool.profiles.iter().find(|p| p.label == "old-daily").expect("old-daily");
    let new = pool.profiles.iter().find(|p| p.label == "new-daily").expect("new-daily");
    assert!(!old.is_main, "only one profile may be main");
    assert!(new.is_main);
    pool.validate().expect("recomputed priorities must satisfy validate()");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p codex-quota-proxy upsert_`
Expected: FAIL to compile — `no method named upsert_profile found for struct PoolSettings`.

- [ ] **Step 3: Implement `upsert_profile`**

In `codex-rs/quota-proxy/src/settings.rs`, inside `impl PoolSettings`, add:

```rust
/// Adds a new account or updates an existing one (matched by exact label),
/// then fully recomputes every profile's `priority` so the invariants
/// `PoolSettings::validate` enforces always hold, no matter what order
/// accounts were added or re-added in. See the design doc for why this is a
/// full recompute rather than an incremental patch.
pub fn upsert_profile(&mut self, label: &str, home: PathBuf, is_main: bool) -> Result<()> {
    validate_label(label)?;

    if let Some(existing) = self
        .profiles
        .iter()
        .find(|profile| profile.label.eq_ignore_ascii_case(label) && profile.label != label)
    {
        bail!(
            "account label '{label}' collides with existing account '{}' (labels differ only by case)",
            existing.label
        );
    }

    if is_main {
        for profile in &mut self.profiles {
            profile.is_main = false;
        }
    }

    match self.profiles.iter_mut().find(|profile| profile.label == label) {
        Some(existing) => {
            existing.home = home;
            existing.is_main = is_main;
        }
        None => self.profiles.push(ProfileSettings {
            label: label.to_string(),
            home,
            priority: 0,
            switch_at_percent: None,
            is_main,
        }),
    }

    self.recompute_priorities();
    Ok(())
}

fn recompute_priorities(&mut self) {
    let non_main_count = self.profiles.iter().filter(|profile| !profile.is_main).count() as u32;
    let mut next_non_main = 0;
    for profile in &mut self.profiles {
        if profile.is_main {
            profile.priority = non_main_count;
        } else {
            profile.priority = next_non_main;
            next_non_main += 1;
        }
    }
}
```

Add `use std::path::PathBuf;` if not already imported at file scope (it already is, for `ProfileSettings::home`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p codex-quota-proxy --lib settings`
Expected: PASS (all Task 1-5 tests)

- [ ] **Step 5: Run the full existing test suite to check for regressions**

Run: `cargo test -p codex-quota-proxy`
Expected: PASS, no regressions in `usage_tests`, `selfcheck_tests`, or any other existing test.

- [ ] **Step 6: Commit**

```bash
git add codex-rs/quota-proxy/src/settings.rs codex-rs/quota-proxy/src/settings_tests.rs
git commit -m "feat(multi-codex): add PoolSettings::upsert_profile with priority recompute"
```

---

### Task 6: Make `resolve_codex_binary` reusable outside `selfcheck.rs`

**Files:**
- Modify: `codex-rs/quota-proxy/src/selfcheck.rs:276-287`
- Modify: `codex-rs/quota-proxy/src/lib.rs`

No new test — this is a visibility change only; existing `selfcheck` tests already cover its behavior indirectly and must keep passing.

- [ ] **Step 1: Widen visibility**

In `codex-rs/quota-proxy/src/selfcheck.rs`, change:

```rust
fn resolve_codex_binary() -> PathBuf {
```
to
```rust
pub fn resolve_codex_binary() -> PathBuf {
```

- [ ] **Step 2: Re-export from the crate root**

In `codex-rs/quota-proxy/src/lib.rs`, add alongside the other `selfcheck` re-exports:

```rust
pub use selfcheck::resolve_codex_binary;
```

- [ ] **Step 3: Verify the crate still builds and existing tests still pass**

Run: `cargo test -p codex-quota-proxy`
Expected: PASS, identical results to Task 5's run.

- [ ] **Step 4: Commit**

```bash
git add codex-rs/quota-proxy/src/selfcheck.rs codex-rs/quota-proxy/src/lib.rs
git commit -m "refactor(multi-codex): expose resolve_codex_binary for reuse by the multi-codex binary"
```

---

## Chunk 2: the `multi-codex` binary

### Task 7: Wire up the new `[[bin]]` target with a stub `main`

**Files:**
- Modify: `codex-rs/quota-proxy/Cargo.toml`
- Create: `codex-rs/quota-proxy/src/bin/multi_codex.rs`

- [ ] **Step 1: Add the `dirs` dependency and the second `[[bin]]`**

In `codex-rs/quota-proxy/Cargo.toml`, add a second `[[bin]]` block right after the existing one:

```toml
[[bin]]
name = "codex-quota-proxy"
path = "src/main.rs"

[[bin]]
name = "multi-codex"
path = "src/bin/multi_codex.rs"
```

Add `dirs` to `[dependencies]` (it is already a workspace dependency used by `codex-utils-home-dir`, version pin lives in the workspace root):

```toml
dirs = { workspace = true }
```

- [ ] **Step 2: Write a stub `main`**

Create `codex-rs/quota-proxy/src/bin/multi_codex.rs`:

```rust
fn main() {
    println!("multi-codex {}", env!("CARGO_PKG_VERSION"));
}
```

- [ ] **Step 3: Verify it builds and runs**

Run: `cargo build -p codex-quota-proxy --bin multi-codex`
Expected: builds with no errors.

Run: `cargo run -p codex-quota-proxy --bin multi-codex`
Expected output: `multi-codex 0.1.0` (or whatever `CARGO_PKG_VERSION` currently is).

- [ ] **Step 4: Commit**

```bash
git add codex-rs/quota-proxy/Cargo.toml codex-rs/quota-proxy/src/bin/multi_codex.rs
git commit -m "feat(multi-codex): add multi-codex binary target"
```

---

### Task 8: Path resolution and `accounts` subcommand

**Files:**
- Modify: `codex-rs/quota-proxy/src/bin/multi_codex.rs`

This task is not unit-tested: it is a thin, real-filesystem-facing layer over the tested `settings.rs` functions from Chunk 1. It is proven by running it, per Task 12.

- [ ] **Step 1: Replace the stub with path resolution and full subcommand dispatch skeleton**

Replace the contents of `codex-rs/quota-proxy/src/bin/multi_codex.rs`:

```rust
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
    todo!("Task 9")
}

async fn setup() -> Result<()> {
    todo!("Task 10")
}

async fn launch() -> Result<()> {
    todo!("Task 11")
}
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p codex-quota-proxy --bin multi-codex`
Expected: builds with no errors (the three `todo!()` bodies are fine — they only panic if called).

- [ ] **Step 3: Real-run verify `accounts` on a fresh machine state**

```bash
rm -rf /tmp/multi-codex-scratch  # only if this exact scratch dir exists from a prior manual run
MULTI_CODEX_HOME=/tmp/multi-codex-scratch cargo run -p codex-quota-proxy --bin multi-codex -- accounts
```

`MULTI_CODEX_HOME` is read directly by `multi_codex_home()` before falling back to `dirs::home_dir()` — use it for every manual verification run in this task and in Task 12 so real account data under the real `~/.multi-codex` is never touched by a test run.

Expected: prints `no accounts configured yet; run \`multi-codex login <label>\`` and creates `/tmp/multi-codex-scratch/pool.toml` with `default_switch_at_percent = 80.0` and `profile = []`.

- [ ] **Step 4: Commit**

```bash
git add codex-rs/quota-proxy/src/bin/multi_codex.rs
git commit -m "feat(multi-codex): add path resolution, accounts/check/status/selfcheck subcommands"
```

---

### Task 9: `login` subcommand

**Files:**
- Modify: `codex-rs/quota-proxy/src/bin/multi_codex.rs`

Not unit-tested: shells out to a real, interactive `codex login`. Proven by real runs in Task 12, including the missing-binary and cancelled-login error paths the design doc calls out explicitly.

- [ ] **Step 1: Implement `login`**

Replace the `login` stub in `codex-rs/quota-proxy/src/bin/multi_codex.rs`:

```rust
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
```

- [ ] **Step 2: Export `validate_label` from the crate root**

In `codex-rs/quota-proxy/src/lib.rs`, add:

```rust
pub use settings::validate_label;
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build -p codex-quota-proxy --bin multi-codex`
Expected: builds with no errors.

- [ ] **Step 4: Commit**

```bash
git add codex-rs/quota-proxy/src/bin/multi_codex.rs codex-rs/quota-proxy/src/lib.rs
git commit -m "feat(multi-codex): add login subcommand"
```

(Real-run verification of `login`, including its error paths, happens in Task 12 — it needs a real terminal and cannot be scripted.)

---

### Task 10: `setup` subcommand

**Files:**
- Modify: `codex-rs/quota-proxy/src/bin/multi_codex.rs`

- [ ] **Step 1: Implement `setup`**

Replace the `setup` stub:

```rust
async fn setup() -> Result<()> {
    use std::io::BufRead;
    use std::io::Write;

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();

    loop {
        print!("Account label (blank to finish): ");
        std::io::stdout().flush().ok();
        let Some(line) = lines.next() else { break };
        let label = line.context("could not read account label")?;
        let label = label.trim();
        if label.is_empty() {
            break;
        }

        print!("Is this the main fallback account? [y/N]: ");
        std::io::stdout().flush().ok();
        let Some(answer) = lines.next() else { break };
        let answer = answer.context("could not read main-account answer")?;
        let is_main = matches!(answer.trim().to_lowercase().as_str(), "y" | "yes");

        login(label, is_main).await?;
    }

    println!("setup finished; run `multi-codex accounts` to review.");
    Ok(())
}
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p codex-quota-proxy --bin multi-codex`
Expected: builds with no errors.

- [ ] **Step 3: Commit**

```bash
git add codex-rs/quota-proxy/src/bin/multi_codex.rs
git commit -m "feat(multi-codex): add setup subcommand"
```

(Real-run verification happens in Task 12, same as `login`.)

---

### Task 11: Default launch — start the proxy and spawn real interactive Codex

**Files:**
- Modify: `codex-rs/quota-proxy/src/bin/multi_codex.rs`

This is the one piece the design doc flags as genuinely new risk: spawning a real interactive `codex` as a child with inherited stdio. Not unit-testable; proven by hand in Task 12 on both Windows and Unix.

- [ ] **Step 1: Implement `launch`**

Replace the `launch` stub:

```rust
async fn launch() -> Result<()> {
    let pool_toml = pool_toml_path()?;
    PoolSettings::ensure_exists(&pool_toml)?;
    let settings = PoolSettings::load(&pool_toml)?;
    settings.validate()?;

    let credentials = settings.load_credentials().await;
    for error in &credentials.errors {
        eprintln!("{error}");
    }
    if credentials.loaded.is_empty() {
        bail!("no accounts have working credentials; run `multi-codex login <label>` first");
    }

    let listen_addr = settings.listen_addr.clone();
    let usage_path = usage_path_for(&pool_toml);
    let loaded_accounts = credentials.loaded;

    // `settings` moves into this future and lives exactly as long as the
    // spawned task needs it — no `Arc` required, since nothing outside this
    // task still needs to read it afterward.
    let mut serve_task =
        tokio::spawn(async move { serve(&settings, loaded_accounts, usage_path).await });

    tokio::select! {
        joined = &mut serve_task => {
            let result = joined.context("proxy task panicked before it could start")?;
            return result.context(
                "could not start the proxy — is another multi-codex or codex-quota-proxy already running on this port?",
            );
        }
        _ = tokio::time::sleep(Duration::from_millis(300)) => {}
    }

    let codex_binary = resolve_codex_binary();
    let base_url = format!("http://{listen_addr}/backend-api/codex");
    let mut child = tokio::process::Command::new(&codex_binary)
        .arg("-c")
        .arg("model_providers.multi_codex.name=\"multi_codex\"")
        .arg("-c")
        .arg(format!("model_providers.multi_codex.base_url=\"{base_url}\""))
        .arg("-c")
        .arg("model_providers.multi_codex.wire_api=\"responses\"")
        .arg("-c")
        .arg("model_providers.multi_codex.requires_openai_auth=true")
        .arg("-c")
        .arg("model_providers.multi_codex.supports_websockets=false")
        .arg("-c")
        .arg("model_provider=\"multi_codex\"")
        .spawn()
        .with_context(|| {
            format!(
                "could not run '{}' — is codex installed and on PATH?",
                codex_binary.display()
            )
        })?;

    let status = child.wait().await.context("could not wait for codex to exit")?;
    serve_task.abort();

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p codex-quota-proxy --bin multi-codex`
Expected: builds with no errors.

- [ ] **Step 3: Commit**

```bash
git add codex-rs/quota-proxy/src/bin/multi_codex.rs
git commit -m "feat(multi-codex): add default launch path (proxy + real interactive codex child)"
```

---

### Task 12: Real-run verification (manual, per the design doc — cannot be scripted)

**No files change in this task.** This task is the load-bearing proof the design doc requires before this can be considered done. Record what was actually observed in `codex-rs/quota-proxy/EPIC10_RESULTS.md` (new file), same evidence-file convention as `EPIC9_RESULTS.md`.

**Every step below must run with `MULTI_CODEX_HOME` set to a scratch directory** (e.g. `MULTI_CODEX_HOME=/tmp/multi-codex-scratch`, or a fresh one per step) — this is the only override `multi_codex_home()` consults (Task 8). `HOME`/`USERPROFILE` have **no effect**: `dirs::home_dir()` reads platform APIs directly (`SHGetKnownFolderPath` on Windows) and ignores both. Running any step below without `MULTI_CODEX_HOME` set operates on the real `~/.multi-codex` instead of a scratch copy — confirm it is set (e.g. `echo $MULTI_CODEX_HOME` / `$env:MULTI_CODEX_HOME` in PowerShell) before each command.

- [ ] **Step 1:** Corrupted `pool.toml` fails closed. With `MULTI_CODEX_HOME` pointed at a fresh scratch directory, write a syntactically-broken `pool.toml` there and run `multi-codex accounts`. Confirm it exits non-zero with `PoolSettings::load`'s existing line-numbered error, and does **not** silently fall back to an auto-created empty file.
- [ ] **Step 2:** `multi-codex login <label>` happy path against a real account, with `MULTI_CODEX_HOME` pointed at a fresh scratch directory. Confirm the OAuth/device-code flow works exactly as plain `codex login` does, and that `pool.toml` gains the expected `[[profile]]` block afterward.
- [ ] **Step 3:** `multi-codex login` with the `codex` binary temporarily unavailable (e.g. rename it or clear PATH for the command), `MULTI_CODEX_HOME` still pointed at a scratch directory. Confirm a clear error, not a panic or hang.
- [ ] **Step 4:** `multi-codex login` with a cancelled/failed login (e.g. Ctrl-C the device-code prompt), `MULTI_CODEX_HOME` still pointed at a scratch directory. Confirm a clear non-zero exit, not a panic, and that `pool.toml` is not corrupted or given a bogus entry.
- [ ] **Step 5:** `multi-codex setup` end-to-end with two scratch accounts, one marked main, `MULTI_CODEX_HOME` pointed at a scratch directory. Confirm `multi-codex accounts` shows both with `daily (main)` at the highest priority.
- [ ] **Step 6:** Default `multi-codex` launch on **this project's Windows dev machine**, `MULTI_CODEX_HOME` pointed at a scratch directory with at least one real logged-in account: confirm the proxy starts, the real interactive Codex TUI appears, a normal chat turn works, Ctrl-C/exit behaves normally, and the proxy process ends when Codex exits.
- [ ] **Step 7:** Attempt the same default-launch verification on a Unix shell if one is available in this environment (there, `MULTI_CODEX_HOME` is just as required, even though `dirs::home_dir()` *would* otherwise read `$HOME` on Unix — keep the override for consistency and to avoid touching a real `~/.multi-codex` there too). If no Unix shell is available, say so explicitly in `EPIC10_RESULTS.md` as an open caveat — do not claim Unix coverage without having actually run it.
- [ ] **Step 8:** Port-already-in-use case: start `multi-codex` once (scratch `MULTI_CODEX_HOME`), then start a second instance against the same scratch `pool.toml` while the first is still running. Confirm the second exits with a clear startup error rather than hanging or silently double-serving.
- [ ] **Step 9:** Write up all of the above, with exact commands and observed output (redacting any tokens), in `codex-rs/quota-proxy/EPIC10_RESULTS.md`.
- [ ] **Step 10:** `git status` — confirm only the intended files changed (settings.rs, settings_tests.rs, lib.rs, selfcheck.rs, Cargo.toml, bin/multi_codex.rs, EPIC10_RESULTS.md). Delete every scratch directory used as `MULTI_CODEX_HOME` for these manual runs; none must be committed.
- [ ] **Step 11: Commit**

```bash
git add codex-rs/quota-proxy/EPIC10_RESULTS.md
git commit -m "docs(multi-codex): record real-run verification of login and launch"
```

---

## Notes for whoever executes this plan

- Chunks 1 and 2 are TDD end-to-end except Task 12, which the design doc explicitly calls out as unprovable by unit test.
- If Task 12 surfaces a real bug (not a documentation gap), stop and fix it with its own failing-test-first cycle before continuing, same as any other bug found during implementation.
- Nothing in this plan touches `~/.codex/config.toml`; the override is always a process-local `-c` flag on the spawned `codex`/`codex exec` child, matching the design doc's explicit "no config.toml editing at all" decision.
- Do not commit, push, or delete branches beyond what each task's own `git commit` step says — that mirrors the standing rule from every earlier epic in this project.
