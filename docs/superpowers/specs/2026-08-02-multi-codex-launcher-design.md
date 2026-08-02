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
  auto-created the first time any `multi-codex` command runs and it's
  missing, with an explicit `default_switch_at_percent = 80.0` and an empty
  `profile = []` (both required for `PoolSettings::load` to parse it — a
  file with no profile blocks at all is not the same as an explicit empty
  array, so the auto-created starter file must write the empty array
  literally).
- Per-account credentials: `~/.multi-codex/accounts/<label>/`, one directory
  per account, auto-created by `login`. `<label>` is restricted to
  `[A-Za-z0-9_-]` (rejected otherwise with a clear error) — simple enough to
  rule out path separators, reserved Windows device names (`CON`, `NUL`,
  etc.), and case-only collisions without needing a general path-sanitizer.
- Usage file: same convention as today, next to `pool.toml`.

## How it's built

New thin binary target `multi-codex` inside the existing `codex-quota-proxy`
crate (`Cargo.toml` gets a second `[[bin]]`, pointing at a new
`src/bin/multi_codex.rs`) — not a new crate, so it reuses every existing
module directly. Its subcommand dispatch mirrors `main.rs`'s today, plus:

- `login`/`setup`: shell out to the real `codex login` (inherits the
  terminal, so the existing OAuth/device-code flow works exactly as it does
  today), then read-modify-write `pool.toml` to add or update that account's
  `[[profile]]` entry. **This is new work, not a reuse of existing code**:
  `PoolSettings`/`ProfileSettings` currently derive `Deserialize` only (load
  is read-only today), so this needs a `Serialize` impl added and a small
  writer helper in `settings.rs`. Priority assignment is **fully
  recomputed on every write**, not incrementally patched, to keep
  `PoolSettings::validate()`'s invariant (unique priorities, main = strictly
  highest) true no matter what order accounts are added or re-added in: every
  non-main profile gets priority `0..N-1` in the order they appear in the
  file, and the main profile (if one exists) always gets priority `N` —
  recomputed fresh each time, so adding a new account after a main already
  exists can never push past it.
- Both `login`/`setup` and the default launch path need to find the real
  `codex` executable. `selfcheck.rs` already has `resolve_codex_binary()`,
  but it's private and `multi-codex` is a separate binary target that only
  sees the crate's public surface — so this needs to become a small `pub`
  function (moved to `lib.rs` or re-exported), shared by both, not
  duplicated.
- Default (no subcommand): starts the proxy in-process (`proxy::serve`,
  already exists, no changes needed) on the settings file's configured
  `listen_addr`, then spawns the real `codex` binary as a child process with
  **inherited stdio** (so the interactive TUI behaves normally — Ctrl-C,
  resizing, etc.) and `-c` provider-override flags pointing it at the
  freshly-started proxy — the same override mechanism `selfcheck` already
  uses for `codex exec`, just applied to a real interactive session instead
  of a one-shot prompt. This means **no `config.toml` editing at all**,
  managed or otherwise — the override is process-local and vanishes when
  Codex exits. Waits for the child to exit, then stops the proxy. If the
  configured port is already bound (a previous `multi-codex` still running,
  or a stale process), this is a clear startup error, same as trying to
  start `codex-quota-proxy` twice today — not a new case to design around.

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

- Removing an account, or editing `switch_at_percent` after the fact — still
  hand-edit `pool.toml`, which still exists, just isn't hand-*written* from
  scratch anymore. **`priority` is the one field this doesn't apply to**: it's
  fully recomputed by file order on every `login`/`setup` call (see above), so
  a hand-edited priority ordering only survives until the next time either
  command runs, at which point it's silently renumbered back to insertion
  order. Reordering `[[profile]]` blocks in the file (rather than editing
  their `priority` numbers) is the durable way to hand-adjust order, since
  that's what the recompute reads — though the reorder only takes effect the
  *next* time `login`/`setup` runs and rewrites the file; until then, runtime
  selection (`AccountSelector::new`, which sorts by the numeric `priority`
  field already in the file) still uses whatever priorities are currently
  written there.
- The `/login` slash command inside Codex's own chat UI (Option B) — not
  ruled out for later, just not this pass.

## Testing

- `pool.toml` auto-create-if-missing (including the explicit-empty-array
  case) and the `login` read-modify-write logic — including the priority
  recompute across repeated adds (non-main then main, main then non-main,
  re-adding an existing label) and label rejection for invalid characters:
  unit tests, temp-dir based, matching this crate's existing test style.
- The account-management subcommands (`login`, `setup`, `accounts`): unit
  tests where possible (the TOML patching), real-run verification for the
  parts that shell out to `codex login` (can't script a real OAuth flow) —
  including the error paths of a missing `codex` binary and a
  cancelled/failed login, both surfaced as clear errors rather than a panic.
- A corrupted/malformed `pool.toml` fails closed with a clear, line-numbered
  error — exactly what `PoolSettings::load` already does today for any
  invalid settings file, zero new code required. This is deliberately the
  opposite of the usage file's behavior (`UsageStore::load` fails *open*,
  silently continuing with an empty cache) — that's appropriate for
  disposable usage data, but wrong here: silently falling through to an
  auto-created empty `pool.toml` would look like the user's configured
  accounts had vanished, with no error at all. Reusing the existing
  hard-error path is the correct choice, not a coincidental parallel to draw
  with the usage file.
- The interactive-launch path: proven by actually running `multi-codex` and
  using the resulting Codex session on **both Windows (this project's
  primary dev platform) and a Unix shell**, not by an automated test — since
  stdio inheritance and Ctrl-C/signal handling diverge between the two,
  documented as manually verified on both rather than claimed as covered by
  one platform alone.
