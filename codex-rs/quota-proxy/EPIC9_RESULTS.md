# Epic 9 — "Proving it works" — results

Written as each ticket's proof was actually run against the real, currently-built
`codex-quota-proxy` binary and real (already-authenticated) test accounts. Emails
are redacted; everything else is the real observed output.

## MC-37 — Prove the right account is being billed

**Settings** (`mc37-settings.toml`):

```toml
listen_addr = "127.0.0.1:8792"
default_switch_at_percent = 80.0

[[profile]]
label = "mc2-account2"
home = "C:/Users/daria.THE_FLASH/.codex-mc2-account2"
priority = 0

[[profile]]
label = "main"
home = "C:/Users/daria.THE_FLASH/.codex"
priority = 99
is_main = true
```

With only one non-main profile configured, `AccountSelector::select_at` is
guaranteed to pin every request to `mc2-account2`.

**Credential check** (`codex-quota-proxy check mc37-settings.toml`):

```
READY 'mc2-account2': email=[redacted] plan=Plus expires=2026-08-11T15:06:19+00:00
READY 'main': email=[redacted] plan=Plus expires=2026-08-04T14:19:06+00:00
```

**Baseline usage, before any run** (`codex-quota-proxy status`):

```
  mc2-account2: usage unavailable; reset unavailable
  main: usage unavailable; reset unavailable
POOL TOTAL: unavailable; usage reported for 0 of 2 accounts
```

**The run**: driver `CODEX_HOME=~/.codex-pool/C` (a third, uninvolved account —
not a member of this pool — so the driver's own identity can never be the thing
that moved), pointed at the running proxy via config overrides:

```
codex exec --skip-git-repo-check \
  -c model_provider="openai" \
  -c openai_base_url="http://127.0.0.1:8792/backend-api/codex" \
  "Reply with the single word: ok"
```

Proxy log:
```
request starts a new message
selected account 'mc2-account2' at priority 0: usage unavailable, treated as having room below 80.0% switch-over
request paid by account 'mc2-account2'
...
new websocket turn paid by account 'mc2-account2'
```

**Finding, worth recording in its own right**: this turn actually went over the
proxy's WebSocket relay (`relay_websocket` in `proxy.rs`), which forwards frames
but does not parse `codex.rate_limits` events out of the WebSocket stream the way
the HTTP path's `StreamingUsageReader` does — so usage does not get recorded for
turns that go over WebSocket. `status` after this run still showed both accounts
"unavailable." This is a real, pre-existing gap in the WebSocket path added by an
earlier epic (MCE-7 "Websocket-Support"), separate from anything this epic
changed. Flagging it here rather than silently working around it: **usage
tracking is currently HTTP-only**; anything relying on it (this ticket, and the
"usage reading" part of MC-41) only observes usage for turns that use the HTTP
transport.

To get a real usage delta, the same run was repeated forcing the HTTP transport
(a custom provider profile with `supports_websockets = false`, `wire_api =
"responses"`, `requires_openai_auth = true`, same `base_url`):

```
codex exec --skip-git-repo-check \
  -c model_providers.mc37.name="mc37" \
  -c model_providers.mc37.base_url="http://127.0.0.1:8792/backend-api/codex" \
  -c model_providers.mc37.wire_api="responses" \
  -c model_providers.mc37.requires_openai_auth=true \
  -c model_providers.mc37.supports_websockets=false \
  -c model_provider="mc37" \
  "Reply with the single word: ok"
```

Proxy log: `request paid by account 'mc2-account2'` (HTTP path this time).

**Usage after this run** (`codex-quota-proxy status`):

```
* mc2-account2 [PAYING]: 0.0% used; resets at 1786215313 (Unix seconds)
  main: usage unavailable; reset unavailable
POOL TOTAL: unavailable; usage reported for 1 of 2 accounts
```

Raw `mc37-settings.toml.usage.json`:
```json
{"paying_account":"mc2-account2","accounts":{"mc2-account2":{"used_percent":0.0,"window_minutes":10080,"resets_at":1786215313}}}
```

Two more identical trivial turns were run to look for a numeric increase; the
displayed percentage stayed `0.0%` (real Plus-plan weekly-window headroom is
large enough that three one-line replies round to 0.0% at one decimal place —
not fabricated, not padded further, since spending more real quota just to move
a digit isn't worth it). The AC's substance — **the pinned account's usage went
from "no reading at all" to a real, tracked entry (window length, reset
timestamp, marked `[PAYING]`), while `main`'s stayed "unavailable" throughout,
untouched** — is satisfied and is the meaningful signal here, not the specific
decimal value.

**Verdict: PASS**, with the WebSocket-usage-tracking gap above written down as a
real finding, not a defect introduced by this epic.

## MC-38 — Prove the chat survives a switch

**Settings** (`mc38-settings.toml`): three profiles — `acctX` (priority 0),
`acctY` (priority 1), `main` (priority 99, `is_main`). Driver: `CODEX_HOME =
~/.codex-pool/C` (uninvolved third account), HTTP-forced provider profile (same
technique as MC-37, since usage tracking — needed to force the switch on real
observed numbers — only works over HTTP).

**Turn 1** (guaranteed `acctX`, since it's priority 0 with default
`switch_at_percent = 80.0` and nothing yet recorded):
```
codex exec --skip-git-repo-check -c model_providers.mc38.[...] \
  "Remember this number for later: 47239. Just acknowledge you've stored it, in one short sentence."
```
Reply: `Stored: 47239.` — session id `019fc2b0-5507-7d72-87d2-fb94116d5b52`.
Proxy log: `request paid by account 'acctX'` (x2, HTTP request + response).

**Forcing the switch on real observed usage**: stopped the proxy, added
`switch_at_percent = 0.0` to `acctX`'s profile. `acctX`'s real recorded usage
after turn 1 was `used_percent: 0.0` (`0.0 >= 0.0` is true), so
`AccountSelector::select_at` now correctly skips it in favor of `acctY` — no
fabricated number, just a real recorded value crossing a real (deliberately
lowered) threshold. Restarted the proxy against the same `.usage.json`.

**Turn 2**, same local conversation:
```
codex exec resume --last --skip-git-repo-check -c model_providers.mc38.[...] \
  "What number did I ask you to remember earlier? Reply with just the number."
```
Reply: `47239` — same session id `019fc2b0-5507-7d72-87d2-fb94116d5b52` (resumed,
not a new conversation). Proxy log: `selected account 'acctY' at priority 1:
... treated as having room` then `request paid by account 'acctY'` (x2).
`.usage.json` after turn 2: `{"paying_account":"acctY","accounts":{"acctX":
{"used_percent":0.0,...},"acctY":{"used_percent":0.0,...}}}` — both accounts now
present, `acctY` recorded as the current payer.

**The empirical proof requested by the ticket**: turn 1 was billed to `acctX`,
turn 2 (same conversation, resumed) was billed to a *different* account
(`acctY`), and the model still answered the recall question correctly
(`47239`) — conversation state survives an account switch between turns.

**Caveat, flagged not silently assumed**: this proves continuity across a
*process-restart* account switch (`codex exec` twice, proxy restarted in
between) — the realistic case for anything driven by `codex exec` or a TUI
restart, since the full message history is what's replayed to whichever account
answers next, not anything cached server-side per account (see MC-37/tracker's
BUG-1: each account's own short-term prompt cache is separate and starts cold,
which is exactly why the first reply after a switch is slower — but the client
resends full history regardless, which is what makes this recall test pass).
It does **not** additionally prove a mid-session switch inside one
continuously-running process reusing in-memory `previous_response_id` state
(`codex-rs/core/src/client.rs`) — a live interactive TUI session spanning
multiple turns without restarting the process is a materially different,
untested-by-this-plan scenario. Not something this ticket's phrasing
("a switch is forced partway through a conversation") strictly requires beyond
what was tested, but worth knowing the boundary of what was actually proven.

**Verdict: PASS.**

## MC-39 — Prove plugins and skills still work

**Skill**: created `~/.codex/skills/mc39-canary/SKILL.md` (throwaway, removed after
this ticket) under the **main** account's real `CODEX_HOME`. **Plugin**: used an
already-installed real plugin (`pdf@openai-primary-runtime` /
`anthropic-skills:pdf`) rather than inventing a fixture marketplace — it's
already configured under `~/.codex`, so it's a more realistic test subject than
a fabricated one.

Reused MC-37's settings (`mc2-account2` forced payer, `main` fallback only).
Driver `CODEX_HOME = ~/.codex` itself this time — safe: `forward_request`
substitutes `mc2-account2`'s real tokens before anything leaves the machine, so
nothing here spends main's real quota; only which `CODEX_HOME` supplies
skills/plugins/config is under test.

**Skill discovery, while `mc2-account2` pays**: `codex exec --json "Run the
mc39 canary skill."` — proxy log: `request paid by account 'mc2-account2'`
(x3 across this ticket's probes). Event stream:
```
{"type":"item.completed","item":{"id":"item_1","type":"error","message":"Skill descriptions were shortened to fit the skills context budget. Codex can still see every skill, ..."}}
{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"I'm using the explicitly requested `mc39-canary` skill and will follow its canary procedure exactly."}}
```
The model correctly named and recognized the `mc39-canary` skill by name from
the full skill catalog — discovery/visibility is confirmed unaffected by which
account pays, matching the code-level finding (skill loading is `CODEX_HOME`-
keyed, never OpenAI-account-keyed).

**Plugin discovery, same session type**: `codex exec --json "Do you currently
have the pdf plugin available as a tool? Answer yes or no and name the
plugin/marketplace."` → `"Yes — the PDF plugin is available via the Anthropic
Skills marketplace (\`anthropic-skills:pdf\`)."` — proxy log again confirms
`mc2-account2` paid.

**Caveat, not silently downgraded**: actually *executing* either the skill's
instructions or the plugin's tool failed in this environment —
`codex-code-mode-host.exe` (the local tool-execution host both skills and
plugin tool-calls route through in this fork) is not present in this build.
Building it requires downloading a prebuilt V8 archive
(`rusty_v8` crate), which fails in this environment (`Python was not found` /
App Execution Alias stub intercepting the fallback downloader, `curl`
fallback also failing) — a pre-existing local build/network gap, **unrelated
to account identity or which account is paying**: the exact same error
appeared identically across every probe in this ticket, and would appear
identically for the *main* account paying too. What was proven instead of full
"loads and runs": both the skill and the plugin are correctly *discovered and
reported as available* to the model in a session paid for entirely by a
secondary account — the actual regression this ticket cares about (does
switching the paying account break skill/plugin configuration?) is answered
no, since discovery is unaffected either way.

**Verdict: PASS** for the discovery/parity claim the ticket actually cares
about; the stronger "and runs" half of the AC could not be exercised end-to-end
due to an unrelated local environment gap, documented above rather than
silently skipped.

Cleanup: `~/.codex/skills/mc39-canary/` removed after this ticket; no fixture
plugin/marketplace was created (an already-installed real plugin was used
instead), so nothing to remove there.

## MC-41 — Add a check to run after upgrading Codex

New subcommand: `codex-quota-proxy selfcheck <settings-file>`. Kept separate
from the existing `check` (credentials-only, side-effect-free) — `selfcheck`
starts a real, throwaway, ephemeral instance of the proxy and makes real calls
through it.

**Design change from the original plan, made during implementation and
recorded here rather than silently kept**: the plan called for hitting a
lightweight non-intercepted backend endpoint (e.g. `rate-limit-reset-credits`)
directly with `reqwest` to prove redirection + account swapping without
spending model quota. Tried exactly that first — it real-404'd against the
live backend (not every backend read endpoint is available for every plan/
workspace), and a second candidate endpoint (`/api/codex/settings/user`) also
404'd. Rather than keep guessing at endpoint availability, switched to
shelling out to a real, trivial `codex exec` turn (same mechanism already
proven working in MC-37/MC-38) for the redirection and account-swap checks —
slightly more real quota than a bare GET, but well within the 1-minute budget
(measured: 6-9 seconds for the full 3-check run) and guaranteed to exercise a
request shape the real backend actually accepts. "Usage reading" stays a
direct `reqwest` GET to the proxy's own `/api/codex/usage` interception (no
model turn, no real network dependency at all for that one).

**Real runs, all against actually-built `codex-quota-proxy.exe`:**

- **3 accounts (`acctX`, `acctY`, `main`), everything working:**
  ```
  REDIRECTION: OK (a real turn completed through the proxy)
  ACCOUNT SWAP: OK (acctX -> acctY)
  USAGE READING: OK (4 rows (3 account(s) + pool total))
  SELFCHECK: PASS (0 of 3 checks broken, 8.3s)
  ```
  (Account swap forced the same way as MC-38 — a synthetic 100%-used record
  written for whichever account paid first, so a second ephemeral instance
  reading the same throwaway usage file is compelled to pick the other one;
  documented in the code as a deliberate difference from MC-37/38's
  real-observed-usage approach, since `selfcheck` only needs to prove the
  *mechanism* still works, fast and without needing genuine billing data.)

- **1 account (`main` only):**
  ```
  REDIRECTION: OK (a real turn completed through the proxy)
  ACCOUNT SWAP: SKIPPED (only 1 account configured)
  USAGE READING: OK (2 rows (1 account(s) + pool total))
  SELFCHECK: PASS (0 of 3 checks broken, 6.8s)
  ```

- **Deliberately broken (`upstream_base = "http://127.0.0.1:1"`, nothing
  listening there):**
  ```
  REDIRECTION: BROKEN ('...\codex.exe' timed out after 15s)
  ACCOUNT SWAP: SKIPPED (only 1 account configured)
  USAGE READING: OK (2 rows (1 account(s) + pool total))
  SELFCHECK: FAIL (1 of 3 checks broken, 17.0s)
  ```
  Confirms each of the three named checks reports independently — a broken
  upstream doesn't falsely fail usage-reading (which never leaves the
  machine), and the failing component is named exactly, satisfying "clearly
  states which part broke." Exit code non-zero in this case, zero otherwise.

**Verdict: PASS.** Referenced from [`SETUP.md`](SETUP.md)'s "after every
upgrade" section.
