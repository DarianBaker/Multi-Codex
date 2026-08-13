# `multi-codex-wizard` — real-run verification results

Real, observed output from actually running the built `multi-codex-wizard.exe`
(release build) against the real Windows registry and filesystem — never
against the real `PATH` or the real `%LOCALAPPDATA%\multi-codex`. Every run
below used `MULTI_CODEX_WIZARD_TEST_ENV_VAR` to point the wizard at a
throwaway registry value instead of `PATH`, and `MULTI_CODEX_WIZARD_INSTALL_DIR`
to point it at a scratch directory instead of the real per-user install
location. Every scratch value was deleted afterward and independently
confirmed removed.

Built with `cargo build -p codex-quota-proxy --release --bin multi-codex --bin
multi-codex-wizard` — both land in `codex-rs/target/release/` together, which
is how the wizard finds `multi-codex.exe` (looks next to its own `.exe`).

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

## What still needs you

Nothing was run against your real `PATH` or real `%LOCALAPPDATA%\multi-codex`
tonight — that's deliberate. All of the above proves the underlying mechanism
is correct using scratch/throwaway state; actually running the real wizard
(`multi-codex-wizard.exe`, no overrides set) against your real environment is
the one thing left, so you can watch it happen and confirm the result
yourself before it's anything other than reviewable.
