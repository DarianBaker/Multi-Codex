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

## Caveats — what still needs a human, and why

The design doc named this explicitly up front: *"Getting stdio inheritance,
Ctrl-C, and terminal resize to pass through correctly to an interactive child
is the one part that can't be unit-tested — it'll be verified by hand."* That
held. Specifically, these steps from the plan could not be completed from this
non-interactive environment and are **not claimed as done**:

- **Step 2 — real `login` happy path.** The isolated-binary test above
  incidentally *started* a real OAuth flow (see Step 3) and it behaved
  correctly as far as it went — the local login server started, the real
  `https://auth.openai.com/oauth/authorize?...` URL was printed to stdout
  exactly as plain `codex login` does. But completing it requires a browser
  and a human clicking through the real OpenAI login screen; this was not
  carried to completion, and `pool.toml` gaining the expected `[[profile]]`
  block afterward was not observed end-to-end.
- **Step 4 — cancelled/failed login.** Same reason: needs a human to actually
  cancel a real, in-progress OAuth flow (e.g. Ctrl-C at the right moment, or
  closing the browser tab) to observe the error path.
- **Step 5 — `multi-codex setup` end-to-end.** `setup()` calls `login()` in a
  loop, so it has the same real-OAuth dependency as Step 2, twice over.
- **Step 6 (full) — a live interactive Codex session.** Confirmed the proxy
  starts and a correctly-configured `codex` child spawns (see above); did
  **not** confirm a real chat turn, Ctrl-C behavior, or terminal resizing,
  since that requires eyes on a real terminal, not a redirected/piped one.
- **Step 7 — Unix shell verification.** No Unix shell is available in this
  environment (Windows-only dev machine). Not claimed as covered.

**Recommendation:** the remaining steps (2, 4, 5, 6-full, 7) need you, at a
real terminal, with a browser available for the OAuth step. Suggested path:
`multi-codex login <label>` for one throwaway account, `multi-codex accounts`
to confirm it landed, then plain `multi-codex` and use it like a normal Codex
session for a few turns, then Ctrl-C/exit and confirm the proxy process is
gone.

## Cleanup

Every scratch `MULTI_CODEX_HOME` directory and the isolated-binary copy used
above were deleted after use; none were committed. `git status` at the end of
this task shows only the intended source/doc changes.
