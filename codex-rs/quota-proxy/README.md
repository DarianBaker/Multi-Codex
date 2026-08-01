# Point Codex at the quota proxy

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
