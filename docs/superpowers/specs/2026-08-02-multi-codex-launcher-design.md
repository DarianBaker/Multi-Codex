# `multi-codex` launcher — design

## Context

Using the account pool today requires: hand-writing a TOML settings file,
running `codex login` manually per account with `CODEX_HOME` set by hand,
manually starting `codex-quota-proxy` in its own terminal, and manually
pointing `~/.codex/config.toml` at it. The goal of this change is to remove
all of that: a single new binary, `multi-codex`, that manages accounts and
launches Codex already wired to the pool, with no settings file the user ever
hand-writes and no separate proxy process to remember to start.

This is Option A ("wrapper CLI") plus Option C ("guided setup wizard") from
discussion, explicitly chosen over a heavier option (a `/login` slash command
inside the interactive Codex TUI itself) because it needs zero changes to
upstream Codex code and reuses almost everything already built in
`codex-rs/quota-proxy` (`PoolSettings`, `credentials.rs`, `proxy::serve`).

## What changes for the user

- **Add an account**: `multi-codex login work` (add `--main` for the one
  fallback account). Wraps a real `codex login`, no manual `CODEX_HOME`.
- **First-time setup**: `multi-codex setup` — a short prompt loop that asks
  for a label and whether it's the main account, repeated until done. Calls
  the same logic as `login`.
- **See what's configured**: `multi-codex accounts` — lists labels, priority,
  and which one is main.
- **Use it**: `multi-codex` — starts the proxy, launches the real interactive
  `codex` already pointed at it, and shuts the proxy down when Codex exits.
  No settings file path to type, no separate terminal to leave open.
- **Diagnostics**: `multi-codex check` / `status` / `selfcheck` — the
  existing commands, now defaulting to the fixed config path instead of
  requiring a settings-file argument.

## Where things live

- Config: `~/.multi-codex/pool.toml` (Windows: `%USERPROFILE%\.multi-codex\pool.toml`),
  auto-created with sane defaults the first time any `multi-codex` command
  runs and it's missing. Same schema `settings.rs` already defines — nothing
  new to design there.
- Per-account credentials: `~/.multi-codex/accounts/<label>/`, one directory
  per account, auto-created by `login`.
- Usage file: same convention as today, next to `pool.toml`.

## How it's built

New thin binary target `multi-codex` inside the existing `codex-quota-proxy`
crate (`Cargo.toml` gets a second `[[bin]]`, pointing at a new
`src/bin/multi_codex.rs`) — not a new crate, so it reuses every existing
module directly. Its subcommand dispatch mirrors `main.rs`'s today, plus:

- `login`/`setup`: shell out to the real `codex login` (inherits the
  terminal, so the existing OAuth/device-code flow works exactly as it does
  today), then read-modify-write `pool.toml` to add or update that account's
  `[[profile]]` entry (new small helper in `settings.rs`; priorities
  auto-assigned in the order accounts are added, `--main` bumps that account
  above the rest as required by `PoolSettings::validate()`).
- Default (no subcommand): starts the proxy in-process (`proxy::serve`,
  already exists, no changes needed) on a loopback port, then spawns the real
  `codex` binary as a child process with **inherited stdio** (so the
  interactive TUI behaves normally — Ctrl-C, resizing, etc.) and `-c`
  provider-override flags pointing it at the freshly-started proxy — the
  same override mechanism `selfcheck` already uses for `codex exec`, just
  applied to a real interactive session instead of a one-shot prompt. This
  means **no `config.toml` editing at all**, managed or otherwise — the
  override is process-local and vanishes when Codex exits. Waits for the
  child to exit, then stops the proxy.

## What's genuinely new risk (flagged, not hidden)

Everything above reuses proven code except one piece: spawning the *real
interactive* `codex` TUI as a child process with a live terminal. Every
previous use of child-process `codex` in this project (`selfcheck`, MC-37/38)
only ever used the non-interactive `codex exec`. Getting stdio inheritance,
Ctrl-C, and terminal resize to pass through correctly to an interactive child
is the one part that can't be unit-tested — it'll be verified by hand, running
`multi-codex` for real and using it like a normal Codex session, the same way
prior tickets in this epic proved live behavior.

## Out of scope for this pass

- Removing an account, or editing `switch_at_percent`/priority after the fact
  (still hand-edit `pool.toml`, which still exists, just isn't hand-*written*
  from scratch anymore).
- The `/login` slash command inside Codex's own chat UI (Option B) — not
  ruled out for later, just not this pass.

## Testing

- `pool.toml` auto-create-if-missing and the `login` read-modify-write logic:
  unit tests, temp-dir based, matching this crate's existing test style.
- The account-management subcommands (`login`, `setup`, `accounts`): unit
  tests where possible (the TOML patching), real-run verification for the
  parts that shell out to `codex login` (can't script a real OAuth flow).
- The interactive-launch path: proven by actually running `multi-codex` and
  using the resulting Codex session, not by an automated test — documented as
  such rather than claimed as covered.
