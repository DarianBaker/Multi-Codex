# Point Codex at the quota proxy

## Quick start: `multi-codex`

The simplest way to use the account pool — no settings file to write, no
separate proxy process to start, no `config.toml` editing:

```bash
multi-codex login work        # log into an account, repeat for each one
multi-codex login daily --main  # your regular fallback account
multi-codex                   # starts the pool and launches Codex, wired up
```

That's it. `multi-codex accounts` lists what's configured; run `multi-codex`
again any time to use the pool. See [`MULTI_CODEX.md`](MULTI_CODEX.md) for the
full command reference (the setup wizard, checking status, health checks,
building it if `multi-codex` isn't found as a command yet). Everything below
this point is the manual setup path (hand-written settings file, separate
proxy process, `config.toml` editing) — still supported, but `multi-codex`
does all of it for you.

## Manual setup

Start `codex-quota-proxy` with its pool settings file and leave it running. These instructions assume its default listen address, `127.0.0.1:8788`.

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

## Undo

Remove the `model_provider` and `openai_base_url` lines shown above, then fully quit and restart Codex. Codex will use its normal OpenAI endpoint again.
