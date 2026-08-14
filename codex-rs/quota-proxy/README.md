# codex-quota-proxy

Pools several Codex/ChatGPT accounts behind one local proxy so your usage is
spread across all of them instead of one account alone, with automatic
switch-over once an account gets close to its limit. Codex talks to the proxy
exactly like it talks to the real backend — nothing about how you use Codex
changes, only which account ends up paying for each request.

Two ways to use it:

- **`multi-codex`** — a single binary that manages accounts and starts
  everything for you. No settings file to write, no separate process to
  start, no `config.toml` editing. **Recommended.**
- **Manual setup** — hand-write a settings file, run `codex-quota-proxy`
  yourself, edit `~/.codex/config.toml` yourself. More control, more steps.

This doc covers installation and the everyday `multi-codex` user guide. See
[`MULTI_CODEX.md`](MULTI_CODEX.md) for the full command reference,
[`SETUP.md`](SETUP.md) for the manual path, and
[`EPIC9_RESULTS.md`](EPIC9_RESULTS.md) / [`EPIC10_RESULTS.md`](EPIC10_RESULTS.md) /
[`EPIC11_RESULTS.md`](EPIC11_RESULTS.md) for real, observed proof that all of
this actually works — billing the right account, surviving an account switch
mid-conversation, not breaking skills or plugins, a real login/setup/launch
through `multi-codex` itself, and the installer wizard.

## Installation

`multi-codex` is built from this repo, not installed separately. From
`codex-rs/`:

```bash
cargo build -p codex-quota-proxy --release --bin multi-codex
```

That produces `codex-rs/target/release/multi-codex.exe` (or `multi-codex` on
macOS/Linux). Add that folder to your `PATH` once, then open a fresh terminal —
either by hand, or with the Windows installer wizard:

```bash
cargo build --release -p codex-quota-proxy -p codex-cli --bin codex-quota-proxy --bin multi-codex --bin multi-codex-wizard --bin codex
codex-rs\target\release\multi-codex-wizard.exe
```

Building `codex` too (not just `multi-codex`) lets the wizard bundle it into
the install folder, so the result works even with no Codex CLI installed
separately. Copies everything to `%LOCALAPPDATA%\multi-codex\bin`, adds that
folder to your `PATH`, and offers to run `multi-codex setup` right there.
Safe to re-run. See `EPIC11_RESULTS.md` for what's been verified about it.

By hand instead:

- **Windows:** System Properties → Environment Variables → add
  `...\codex-rs\target\release` to your user `PATH`. Or, in an elevated
  PowerShell:
  ```powershell
  [Environment]::SetEnvironmentVariable("PATH", $env:PATH + ";C:\path\to\codex-rs\target\release", "User")
  ```
- **macOS/Linux:** add to your shell profile (`.bashrc`, `.zshrc`, etc.):
  ```bash
  export PATH="$PATH:/path/to/codex-rs/target/release"
  ```

After that, `multi-codex` works as a bare command everywhere. Until you've
done this, use the full path to the built binary directly.

## User guide

### 1. Add an account

```
multi-codex login <label>
```

Runs the real, normal `codex login` flow — opens your browser (or prints a
URL to open yourself on a headless machine):

```
Starting local login server on http://localhost:1455.
If your browser did not open, navigate to this URL to authenticate:

https://auth.openai.com/oauth/authorize?...

On a remote or headless machine? Use `codex login --device-auth` instead.
```

Once you finish logging in:

```
account 'test-account' saved to C:\Users\...\.multi-codex\pool.toml
```

Add `--main` for your regular, everyday account — used only as a last-resort
fallback once every other account is exhausted:

```
multi-codex login daily --main
```

Repeat for each account you want in the pool. `<label>` may only contain
letters, digits, `_` and `-`.

### 2. Or add several at once: the setup wizard

```
multi-codex setup
```

```
Account label (blank to finish): test-account
Is this the main fallback account? [y/N]: n
...
Account label (blank to finish): daily
Is this the main fallback account? [y/N]: y
...
Account label (blank to finish):
setup finished; run `multi-codex accounts` to review.
```

Leave the label blank to stop. Each account goes through the same real login
flow as step 1 — the wizard is just a loop around it.

### 3. Check what's configured

```
multi-codex accounts
```

```
'test-account': priority=0
'test-account2': priority=1
```

Lower priority numbers are tried first, and `(main)` marks the fallback
account. This is recomputed automatically every time you `login`/`setup`.

### 4. Use it

```
multi-codex
```

No arguments. Starts the pool in the background and launches a real,
interactive Codex session already wired up to it — use it exactly like plain
`codex`. Exit however you normally would; the pool shuts down when Codex
exits.

### 5. See who's paying and how much is used

```
multi-codex status
```

```
* test-account [PAYING]: 0.0% used; resets at 1787265306 (Unix seconds)
  test-account2: usage unavailable; reset unavailable
POOL TOTAL: unavailable; usage reported for 1 of 2 accounts
```

The `*`/`[PAYING]` marker moves as the pool switches accounts. "usage
unavailable" just means that account hasn't handled a request yet — expected,
not an error.

### 6. After every Codex upgrade

```
multi-codex selfcheck
```

Checks redirection, account swapping, and usage reading against a throwaway
copy of your real pool, in under a minute, and states exactly which part
broke, if any. Codex forks change their internal request shape without
notice; this is the fast way to catch it before you notice mid-task.

For everything else — `check` (credential diagnostics), what's out of scope
today (removing an account, editing switch-over thresholds), and more — see
[`MULTI_CODEX.md`](MULTI_CODEX.md).

## Manual setup (advanced)

If you'd rather not use `multi-codex`, the pool works the same way underneath
with a hand-written settings file and a separately-run process — see
[`SETUP.md`](SETUP.md) for the full walkthrough (logging into each account,
writing the settings file, starting `codex-quota-proxy`, adding accounts
later, turning it off).

The one piece specific to this file either way — pointing Codex's own config
at wherever the proxy ends up listening — is below, for reference (`multi-codex`
does this automatically; you only need it for the manual path).

### Point Codex at an already-running proxy

Open the user-level Codex configuration file:

- Windows: `%USERPROFILE%\.codex\config.toml`
- macOS and Linux: `~/.codex/config.toml`

Create the `.codex` directory and `config.toml` file if they do not exist. Add these top-level lines, replacing existing `model_provider` or `openai_base_url` lines:

```toml
model_provider = "openai"
openai_base_url = "http://127.0.0.1:8788/backend-api/codex"
```

Do not put these settings in a project's `.codex/config.toml`; Codex ignores provider redirects there. If the proxy uses a custom `listen_addr`, replace only `127.0.0.1:8788` and keep `/backend-api/codex`.

Fully quit and restart Codex, then start a new task. The proxy confirms routing by printing `request paid by account '<label>'` when Codex sends a request.

### Undo

Remove the `model_provider` and `openai_base_url` lines shown above, then fully quit and restart Codex. Codex will use its normal OpenAI endpoint again.
