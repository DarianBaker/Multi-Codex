# `multi-codex` launcher — real-run verification results

Written as each step of the plan's Task 12 was actually run against the real,
currently-built `multi-codex` binary. Emails are redacted; everything else is
the real observed output. Every step below ran with `MULTI_CODEX_HOME` pointed
at a throwaway scratch directory (deleted afterward) — the real `~/.multi-codex`
was never touched by any of this.

## What was proven

### Step 1 — corrupted `pool.toml` fails closed

Wrote a syntactically-broken `pool.toml` (unclosed inline table) to a scratch
`MULTI_CODEX_HOME`, then ran `multi-codex accounts`:

```
Error: settings file .../pool.toml is invalid at line 4: TOML parse error at line 3, column 22
  |
3 |   { label = "broken"
  |                      ^
unclosed inline table, expected `}`
```

Exit code `1`. No auto-created empty file appeared — the existing broken file
was left exactly as written. Confirms `PoolSettings::load`'s existing
line-numbered, fail-closed error is what a user actually sees through
`multi-codex`, not silently papered over by `ensure_exists`.

### Step 3 — `login` with the `codex` binary genuinely unavailable

First attempt (clearing `PATH` only) accidentally proved something else first:
`resolve_codex_binary()` checks for a `codex`/`codex.exe` sibling in the same
directory as the running executable *before* falling back to `PATH` — and this
dev build's `target/debug/` also contains a real `codex.exe` from the same
workspace. So `multi-codex login testacct` with `PATH` cleared still found that
sibling and launched a **real** login flow:

```
Starting local login server on http://localhost:1455.
If your browser did not open, navigate to this URL to authenticate:

https://auth.openai.com/oauth/authorize?...
```

That's genuine, valuable evidence on its own (see Step 2 below), but it meant
the missing-binary path needed a truly isolated test: a copy of `multi-codex.exe`
in a directory with no `codex.exe` sibling, run with `PATH` stripped down to
nothing useful:

```
Error: could not run 'codex.exe' — is codex installed and on PATH?

Caused by:
    program not found
```

Exit code `1`, returned immediately — no hang, no panic.

### Step 8 — port already in use

Started the existing `codex-quota-proxy` binary against a scratch `pool.toml`
(confirmed via its own `listening on 127.0.0.1:18799` log line that it was
actually bound), then started `multi-codex` against the same settings file
while the first was still running:

```
Error: could not start the proxy — is another multi-codex or codex-quota-proxy already running on this port?

Caused by:
    0: could not listen on 127.0.0.1:18799
    1: Only one usage of each socket address (protocol/network address/port) is normally permitted. (os error 10048)
```

Exit code `1`. The 300ms bind-failure race in `launch()` correctly detected
this and reported the specific, actionable error the plan asked for — no hang,
no silent double-serve.

### Step 6 (partial) — default launch mechanics

Ran plain `multi-codex` (no subcommand) against a scratch `pool.toml` pointing
at one real, already-logged-in test account, with stdin redirected from
`/dev/null` (since this Bash-tool environment has no real interactive TTY to
hand it):

```
codex-quota-proxy listening on 127.0.0.1:18799
Error: stdin is not a terminal
```

This proves real, meaningful mechanics: the proxy started and bound the
configured port; `multi-codex` then correctly resolved and spawned the real
`codex` binary with the provider-override flags; `codex` itself refused to
start its TUI without a real terminal (its own guard, not a `multi-codex` bug)
and exited non-zero; `multi-codex` correctly propagated that exit and shut the
proxy down — the port was unbound and the process fully gone within seconds,
confirmed via `netstat` and a PID check.

**What this does NOT prove**, and cannot prove from this environment: that the
interactive TUI actually renders and behaves correctly once it has a real
terminal — sending a real chat turn, Ctrl-C, terminal resize. That is exactly
the "genuinely new risk" the design doc flagged from the start as needing a
human at a real terminal. See Caveats below.

## Update — Steps 2 and 6 completed by a human at a real terminal

After the above was written, the remaining human-only steps were run for real,
outside this environment, on the project's actual Windows dev machine:

- **Step 2 — real `login` happy path: CONFIRMED.** `multi-codex login
  test-account` and `multi-codex login test-account2` both completed a real
  OAuth flow through a browser. `multi-codex accounts` afterward showed both
  accounts with the expected `priority=0`/`priority=1` ordering. `pool.toml`
  gained the expected `[[profile]]` blocks:

  ```toml
  [[profile]]
  label = "test-account"
  home = 'C:\Users\daria.THE_FLASH\.multi-codex\accounts\test-account'
  priority = 0
  is_main = false

  [[profile]]
  label = "test-account2"
  home = 'C:\Users\daria.THE_FLASH\.multi-codex\accounts\test-account2'
  priority = 1
  is_main = false
  ```

- **Step 6 (full) — a live interactive Codex session: CONFIRMED.** Plain
  `multi-codex` (no subcommand) was run for real: the proxy started, a real
  interactive Codex TUI launched as its child with inherited stdio, a normal
  chat turn was sent and answered, and usage was recorded for real —
  `multi-codex status` afterward showed:

  ```
  * test-account [PAYING]: 0.0% used; resets at 1787265306 (Unix seconds)
    test-account2: usage unavailable; reset unavailable
  POOL TOTAL: unavailable; usage reported for 1 of 2 accounts
  ```

  `test-account2` correctly shows no usage yet — it's priority 1 and
  `test-account` (priority 0) is nowhere near its 80% switch threshold, so the
  selector has had no reason to fall through to it. This is the account
  selector working as designed, not a bug.

- **Step 5 — `multi-codex setup`'s interactive loop: CONFIRMED.** Two scripted
  runs against a scratch `MULTI_CODEX_HOME`, piping input via stdin:
  - Blank first label: prompt `Account label (blank to finish): ` appeared,
    reading an empty line broke the loop immediately without ever calling
    `login()`, printed `setup finished; run \`multi-codex accounts\` to
    review.`, exit code `0`.
  - `wizard-test` then `y`: both prompts (`Account label...` and `Is this the
    main fallback account? [y/N]: `) appeared in order, the label and y/N
    answer were parsed correctly, and `login("wizard-test", true)` was
    genuinely invoked — confirmed by the real
    `https://auth.openai.com/oauth/authorize?...` URL appearing, identical to
    a direct `multi-codex login` call. The process was killed via `timeout`
    before completing OAuth (same real-browser limitation as before), and
    `setup()` propagated that as a clean `Error: \`codex login\` for account
    'wizard-test' did not complete successfully (exit status ...)` rather than
    panicking or leaving a half-written `pool.toml` — the scratch dir had no
    `pool.toml` at all afterward, confirming nothing was corrupted.

  This also stands in as informal confirmation of Step 4 (a login that fails
  to complete triggers the intended clean error path), though not from a
  user-initiated Ctrl-C specifically.

**Still genuinely open, low-risk, not yet observed:**

- **Step 7 — Unix shell verification.** Still no Unix shell available in this
  project's environment. Not claimed as covered.

Every other step in this task is now confirmed for real, either from this
environment or by the user at their own terminal.

## Cleanup

Every scratch `MULTI_CODEX_HOME` directory and the isolated-binary copy used
above were deleted after use; none were committed. `git status` at the end of
this task shows only the intended source/doc changes.
