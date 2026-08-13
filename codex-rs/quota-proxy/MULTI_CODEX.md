# Using `multi-codex`

`multi-codex` is the zero-setup way to use the account pool: no settings file
to hand-write, no separate proxy process to remember to start, no editing
`~/.codex/config.toml`. Everything below is real, observed output from
actually running each command — see [`EPIC10_RESULTS.md`](EPIC10_RESULTS.md)
for the full verification record.

If `multi-codex` isn't found as a bare command, either use the full path to
the built binary, or build a release copy and put it on `PATH` — see the
bottom of this doc.

## Add an account

```
multi-codex login <label>
```

This runs the real, normal `codex login` flow — it opens your browser (or, on
a headless machine, prints a URL to open yourself):

```
Starting local login server on http://localhost:1455.
If your browser did not open, navigate to this URL to authenticate:

https://auth.openai.com/oauth/authorize?...

On a remote or headless machine? Use `codex login --device-auth` instead.
```

Once you finish logging in, it saves the account into `~/.multi-codex/pool.toml`:

```
account 'test-account' saved to C:\Users\...\.multi-codex\pool.toml
```

Add `--main` for your regular, everyday account — the one used only as a
last-resort fallback once every other account is exhausted:

```
multi-codex login daily --main
```

`<label>` may only contain letters, digits, `_` and `-` (it becomes a real
directory name under `~/.multi-codex/accounts/`).

## Add several accounts at once: the setup wizard

```
multi-codex setup
```

Prompts you for one account at a time. Real transcript:

```
Account label (blank to finish): test-account
Is this the main fallback account? [y/N]: n
Starting local login server on http://localhost:1455.
...
account 'test-account' saved to ...\pool.toml
Account label (blank to finish): daily
Is this the main fallback account? [y/N]: y
...
account 'daily' saved to ...\pool.toml
Account label (blank to finish):
setup finished; run `multi-codex accounts` to review.
```

Leave the label blank to stop. Each account you enter goes through the exact
same real `codex login` flow as running `multi-codex login` directly — the
wizard is just a loop around it.

## See what's configured

```
multi-codex accounts
```

```
'test-account': priority=0
'test-account2': priority=1
```

The `(main)` marker appears next to whichever account is the fallback, if any.
Lower priority numbers are tried first — this is recomputed automatically
every time you `login`/`setup`, based on the order accounts were added, so
there's no separate command to reorder them (see the design doc if you want to
hand-edit `pool.toml` directly — priority specifically gets overwritten on the
next `login`/`setup` call).

## See who's actually paying, and how much is used

```
multi-codex status
```

```
* test-account [PAYING]: 0.0% used; resets at 1787265306 (Unix seconds)
  test-account2: usage unavailable; reset unavailable
POOL TOTAL: unavailable; usage reported for 1 of 2 accounts
```

The `*`/`[PAYING]` marker is whichever account most recently handled a
request — it moves as the pool switches accounts. An account shows "usage
unavailable" until it's actually been used at least once; that's expected, not
an error. `POOL TOTAL` only fills in once every configured account has
reported at least once.

## Use it

```
multi-codex
```

No arguments. This starts the pool in the background and launches a real,
normal, interactive Codex session already wired up to it — use it exactly like
you'd use plain `codex`. Exit it the same way you always would (Ctrl-C, or
however you normally end a session); the pool shuts down automatically when
Codex exits.

## Check credentials are still valid

```
multi-codex check
```

Prints `READY '<label>': email=... plan=... expires=...` per account, or a
specific `BROKEN '<label>': ...` reason (missing tokens, expired login, etc.)
telling you which account needs `multi-codex login <label>` run again.

## After a Codex upgrade

```
multi-codex selfcheck
```

Runs a full health check — redirection, account swapping, and usage
reading — against a throwaway copy of your real pool, in under a minute.
Clearly states which part broke, if any. Doesn't touch your real recorded
usage.

## If `multi-codex` isn't a recognized command

It's a binary you build from this repo, not something installed separately.
From `codex-rs/`:

```bash
cargo build -p codex-quota-proxy --release --bin multi-codex
```

That produces `codex-rs/target/release/multi-codex.exe`. Add that folder to
your `PATH` once (System Properties → Environment Variables on Windows, or
`export PATH="$PATH:/path/to/codex-rs/target/release"` in your shell profile
on macOS/Linux), open a fresh terminal, and `multi-codex` works everywhere
after that. Until then, use the full path to the binary directly.

## Not covered here

- Removing an account, or changing how quickly one account hands off to the
  next (`switch_at_percent`) — still a manual edit of `pool.toml`, which still
  exists, it just isn't hand-*written* from scratch anymore.
- Everything above is the `multi-codex` path. The older, fully-manual setup
  (hand-written settings file, separate `codex-quota-proxy` process,
  `config.toml` editing) is still supported and documented in
  [`SETUP.md`](SETUP.md) and [`README.md`](README.md), if you'd rather do it
  that way.
