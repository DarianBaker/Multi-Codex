# Setting up the account pool, from nothing

This walks through everything needed to go from a clean machine to a working
pool of accounts behind `codex-quota-proxy`: logging in each account, writing
the settings file, starting the program, adding an account later, and turning
it all off. See [`EPIC9_RESULTS.md`](EPIC9_RESULTS.md) for real, observed proof
that this actually works — billing the right account, surviving an account
switch mid-conversation, and not breaking skills or plugins.

## 1. Log into each pool account

Every account in the pool needs its own directory acting as its `CODEX_HOME`.
Pick one directory per account and log in:

```bash
CODEX_HOME=/path/to/account-a codex login
CODEX_HOME=/path/to/account-b codex login
```

Never reuse your everyday, daily-driver `CODEX_HOME` (the one your normal
`codex` invocations already use) as a **non-main** pool member — only ever list
it as the pool's `is_main` fallback account (see below), since that's the
account you fall back to when every other account in the pool is exhausted.

## 2. Write the settings file

The settings file is TOML. Every field below is read by
[`settings.rs`](src/settings.rs):

```toml
# Optional — defaults to 127.0.0.1:8788
listen_addr = "127.0.0.1:8788"

# Optional — defaults to https://chatgpt.com/backend-api/codex
upstream_base = "https://chatgpt.com/backend-api/codex"

# Required — the percentage at which an account is skipped in favor of the
# next one, unless a profile below overrides it.
default_switch_at_percent = 80.0

[[profile]]
label = "account-a"                 # shown in logs, `status`, and the usage screen
home = "/path/to/account-a"         # the CODEX_HOME you logged into above
priority = 0                        # lower numbers are tried first
# switch_at_percent = 90.0          # optional per-account override

[[profile]]
label = "account-b"
home = "/path/to/account-b"
priority = 1

[[profile]]
label = "main"
home = "/path/to/your/everyday/.codex"
priority = 99                       # must be the highest priority number of all profiles
is_main = true                      # used only once every other account is exhausted
```

Two hard rules, enforced by `PoolSettings::validate()` (fails fast with a clear
message if violated):
- Every profile's `priority` must be unique.
- The `is_main` profile must hold the **highest** priority number of all
  profiles — it's used last, as the fallback of last resort.

## 3. Start the program

```bash
codex-quota-proxy path/to/settings.toml
```

This validates the settings, loads (and renews, if needed) every account's
credentials, then starts listening. It also creates/loads a usage file next to
your settings file (`settings.usage.json`), which records what each account has
used and who's currently paying — safe to delete if you want to reset it, it
will be recreated empty.

Two read-only inspection commands, useful any time the program isn't (or is)
running:
- `codex-quota-proxy check path/to/settings.toml` — validates every account's
  saved login credentials only (fast, no network beyond token renewal).
- `codex-quota-proxy status path/to/settings.toml` — reads the usage file and
  prints each account's used percentage, reset time, which one is currently
  paying, and a combined pool total.

## 4. Point Codex at it

See [`README.md`](README.md) for the exact `config.toml` lines and how to undo
them — that's the whole redirect, and it hasn't changed here.

## 5. Add a new account to an existing pool

1. Log into a new `CODEX_HOME` directory (step 1 above).
2. Append one more `[[profile]]` block to the settings file with a free
   priority number lower than the `is_main` account's.
3. Restart `codex-quota-proxy`.

The existing usage file is untouched and additive — a newly-added label simply
starts with no cached usage, same as day one for any other account.

## 6. Turn it off / go back to normal

1. Undo the redirect — see the README's **Undo** section (remove the
   `model_provider`/`openai_base_url` lines from `~/.codex/config.toml`, fully
   quit and restart Codex).
2. Stop the `codex-quota-proxy` process itself.

## 7. After every Codex upgrade

Run `codex-quota-proxy selfcheck path/to/settings.toml`. It checks redirection,
account swapping, and usage reading in under a minute and clearly states which
part broke, if any — see its own `--help` for details. Codex forks change
their internal request shape without notice; this is the fast way to find out
if an upgrade broke the setup before you notice it mid-task.

## Troubleshooting

These are expected quirks of how the pool works, not bugs to file:

- **The first reply after an account switch is slower and uses more quota than
  usual.** Each account keeps its own short-term server-side memory of recent
  turns; a freshly-selected account starts cold. Only worth investigating if it
  happens on replies that are *not* right after a switch.
- **An answer fails partway through, right when an account runs out.** The
  account is set aside and the next message uses a different account — but an
  account can't be swapped *mid-answer*, only between messages. Simply resend
  the same message; it will complete on the next account. Lowering that
  account's `switch_at_percent` makes this rarer.
- **The usage screen in Codex shows nothing, or only one account.** Usually
  means the redirect (step 4) isn't actually in effect, or the extra usage rows
  are malformed and being discarded. Run `codex-quota-proxy selfcheck` (step 7)
  first — its `USAGE READING` line will say exactly what's missing.
