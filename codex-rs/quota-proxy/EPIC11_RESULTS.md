# `multi-codex-wizard` — real-run verification results

Real, observed output from actually running the built `multi-codex-wizard.exe`
(release build) against the real Windows registry and filesystem — never
against the real `PATH` or the real `%LOCALAPPDATA%\multi-codex`. Every run
below used `MULTI_CODEX_WIZARD_TEST_ENV_VAR` to point the wizard at a
throwaway registry value instead of `PATH`, and `MULTI_CODEX_WIZARD_INSTALL_DIR`
to point it at a scratch directory instead of the real per-user install
location. Every scratch value was deleted afterward and independently
confirmed removed.

Built with `cargo build --release -p codex-quota-proxy -p codex-cli --bin
multi-codex --bin multi-codex-wizard --bin codex` — all three land in
`codex-rs/target/release/` together, which is how the wizard finds
`multi-codex.exe` and `codex.exe` (looks next to its own `.exe`).

## What was proven

**Unit tests (10, all passing):** the pure PATH-string logic —
`path_contains_dir` (exact match, case-insensitive, trailing-slash
normalization, absent, empty) and `append_dir_if_missing` (appends onto a
populated value, handles a trailing `;` without doubling it, handles an empty
existing value, returns `None` — not a duplicate entry — when already
present, case-insensitively).

**Fresh install, real registry write:**
```
Found multi-codex at ...\target\release\multi-codex.exe
Installed multi-codex to <scratch>\install\multi-codex.exe
Added '<scratch>\install' to your user MULTI_CODEX_WIZARD_TEST_VAR. Open a new terminal for it to take effect.

Would you like to log into an account now? [y/N]: Skipping. Run `multi-codex setup` any time to add accounts.

All done. Open a new terminal and run `multi-codex`.
```
Independently confirmed via a **separate** `powershell.exe` call (not the
wizard's own code) that the scratch registry value actually held the new
directory afterward, and that the file was actually copied.

**Idempotency:** running it again against the same state correctly detected
the directory was already present (`'<scratch>\install' is already on your
MULTI_CODEX_WIZARD_TEST_VAR.`) and did not duplicate the entry — confirmed by
reading the value back afterward.

**Appending onto a realistic, populated value:** pre-set the scratch variable
to `C:\SomeOther\Dir;C:\Another\One`, ran the wizard, confirmed the result was
exactly `C:\SomeOther\Dir;C:\Another\One;<scratch install dir>` — existing
entries preserved verbatim, new one appended, no reordering or corruption.

**Missing-binary error path:** copied only `multi-codex-wizard.exe` into an
isolated directory with no `multi-codex.exe` beside it. Got a clear,
non-panicking error:
```
Error: could not find 'multi-codex.exe' next to this wizard (looked in ...).
Build it first with `cargo build -p codex-quota-proxy --release --bin multi-codex`,
then re-run this wizard from the same target/release directory.
```
Exit code `1`.

**"Add an account now?" hand-off:** answered `y`, confirmed the wizard
genuinely invoked the real, already-tested `multi-codex.exe setup` as a child
process (inherited stdio) — its distinctive `Account label (blank to
finish): ` prompt appeared, which only ever comes from that real subprocess,
not from the wizard's own code. The wizard doesn't reimplement any of the
account-adding logic; it just launches the proven `setup` subcommand.

## Caveat found during testing (not a bug, a note on `write_user_env_var`'s error path)

One run, immediately after a ~4-minute release compile (heavy CPU/disk
contention on the machine at the time), hit `Error: powershell exited with
exit code: 143 while updating '...'` — a `SIGTERM`-style non-zero exit. Reading
the value back afterward showed **the write had actually already succeeded**
before PowerShell was killed during its own trailing cleanup/broadcast phase.

This means `write_user_env_var` can, rarely, report failure for a write that
already durably completed (the registry write itself is synchronous and had
already happened) — not data corruption, and safe to just re-run (the whole
operation is idempotent: `append_dir_if_missing` would correctly report
"already present" on the retry), but worth knowing: if a real run ever prints
that specific error, check `multi-codex` on `PATH` before assuming nothing
happened. Not fixed here since it's a defensive-but-occasionally-overcautious
error report, not a correctness bug — flagging it rather than hiding it.

## Update — a real run against your real PATH, and the `codex.exe` gap it found

You ran the wizard for real (no overrides) shortly after the above was
written. It worked exactly as designed: found `multi-codex.exe`, installed it
to the real `%LOCALAPPDATA%\multi-codex\bin`, added that to your real user
`PATH`. You then said yes to adding an account, and it correctly handed off to
`multi-codex.exe setup` — which failed with `Error: could not run 'codex.exe'
— is codex installed and on PATH?`.

That wasn't a wizard bug: `multi-codex login`/`setup` look for `codex.exe`
next to wherever `multi-codex.exe` is currently running from, which worked
fine in `target/release/` (this repo builds `codex` and `multi-codex`
side by side there) but stopped being true the moment the wizard copied
`multi-codex.exe` to its permanent install folder — a folder with no
`codex.exe` in it. This machine also had no separately-installed, on-`PATH`
Codex CLI to fall back to (`codex-rs/target/release/codex.exe` had never been
built; only a stale `target/debug/codex.exe` existed).

**Fix:** the wizard now also looks for `codex.exe` next to itself and, if
found, copies it into the install folder alongside `multi-codex.exe` — so the
sibling lookup keeps working after install, and the result is self-contained
even on a machine with no separate Codex CLI install at all.

**Verified, real binaries, scratch install dir + scratch registry variable
(never the real `PATH`):**
- With `codex.exe` present next to the wizard (built via `cargo build
  --release -p codex-quota-proxy -p codex-cli --bin multi-codex --bin
  multi-codex-wizard --bin codex`): output included `Also bundled codex from
  ...\target\release\codex.exe`, and both `codex.exe` and `multi-codex.exe`
  were confirmed present in the scratch install directory afterward, correct
  file sizes.
- With no `codex.exe` next to the wizard (isolated directory, only
  `multi-codex-wizard.exe` + `multi-codex.exe`): output included `No codex.exe
  found next to this wizard — assuming the official Codex CLI is already
  installed and on PATH.` — no error, no crash, install proceeds normally for
  people who already have Codex CLI installed separately.

## Update — the proxy's own logging was corrupting the live TUI

You ran `multi-codex` for real on the clean-PC test after the `codex.exe`
bundling fix, and hit a new, worse bug: the interactive Codex session's
display was visibly breaking — proxy log lines like `request starts a new
message`, `selected account '...' at priority 0: ...`, `request paid by
account '...'` appearing mid-screen, interleaved with (and sometimes inside)
what should have been the chat input prompt.

**Root cause:** `launch()` ran the proxy *in-process* via `tokio::spawn` —
reusing the exact same OS process as `multi-codex` itself, which right after
that spawns the real interactive `codex` TUI as a child with **inherited
stdio** (required for the TUI to work at all). The proxy logs a line via
`eprintln!`/`println!` on nearly every request (confirmed by grepping
`proxy.rs`: over a dozen call sites). Since the proxy and `multi-codex` shared
one process, every one of those log lines wrote directly into the same
stdout/stderr the TUI was doing raw-mode terminal rendering into — exactly the
corruption observed. This is the "genuinely new risk" the original design doc
flagged for the interactive-launch path; the risk was real and this is what it
looked like in practice.

**Fix:** the proxy now runs as a genuinely separate OS process
(`codex-quota-proxy.exe <pool.toml>`, spawned by `launch()`), with its
stdout/stderr redirected to `~/.multi-codex/proxy.log` instead of inherited.
`multi-codex`'s own stdio — the thing the `codex` child actually inherits — is
now never touched by the proxy at all. The 300ms immediate-bind-failure check
and the "did the proxy die while codex was running" check were both ported
from polling a `tokio::JoinHandle` to polling the child process directly
(`try_wait()`), same intent, now process-based. The wizard was updated to also
bundle `codex-quota-proxy.exe` alongside `multi-codex.exe`/`codex.exe`, since
`launch()` now needs it as a sibling binary the same way it needs `codex.exe`.

**Verified for real** (scratch `MULTI_CODEX_HOME`, a real already-logged-in
test account, never your real `~/.multi-codex`):
- Ran `multi-codex` (no args) for real: its own captured stdout was
  **completely empty**, stderr contained only the expected `codex` startup
  message (this sandbox has no real TTY, so `codex` itself refused to start
  with `Error: stdin is not a terminal` — a real terminal wouldn't hit this).
  `proxy.log` contained exactly the proxy's real output (`account 'scratch-c':
  ...`, `codex-quota-proxy listening on ...`) — fully separated from the
  terminal, as intended.
- Port-already-in-use case re-verified against the new process-based
  detection: started `codex-quota-proxy.exe` directly against the same
  settings, confirmed it was actually listening, then ran `multi-codex`
  against it — got the same clear `could not start the proxy (exited with
  exit code: 1; see .../proxy.log for details) — is another multi-codex or
  codex-quota-proxy already running on this port?` error, exit code 1, no
  hang.
- Re-ran the wizard against a scratch install dir with all of
  `codex-quota-proxy.exe`/`codex.exe`/`multi-codex.exe` available: confirmed
  all three now get bundled (`Also bundled codex-quota-proxy from ...`
  alongside the existing `codex.exe` message), all three present in the
  install directory afterward with correct file sizes.

**What still needs you:** whether the *live TUI itself* now renders cleanly
with a real terminal — this sandbox genuinely cannot start the interactive
`codex` TUI at all (no TTY), so the strongest claim provable from here is that
the proxy's output is fully isolated from `multi-codex`'s own stdio, which was
the actual mechanism of the corruption. Confirming the screen itself looks
right needs you, at a real terminal, same as every other interactive-TUI claim
in this project.

## What still needs you (bundling fix, from before)

The bundling fix itself hasn't been run against your *real* `PATH`/install
folder yet — only against scratch state, deliberately, same reasoning as
before. Re-running the real wizard (no overrides) will pick up the fix; on a
completely clean machine with no prior Codex install at all, `multi-codex
setup` should now work without you needing to install Codex CLI separately
first.
