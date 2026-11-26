# Plaid Cloud

Hosted recognition and refine for people without their own Volcengine and LLM keys. Users are friends, invited
one by one; BYOK stays the default and keeps working exactly as before.

## Shape

- `app/cloud`, binary `plaid-cloud`, in the app workspace. It reuses plaid-core: the Volc session
  (`session.rs`), the silence gate (`gate.rs`), the refine decision and prompt (`refine.rs`), and the LLM
  request builder and parser (`llm.rs`).
- axum + tokio. The LLM goes through one shared reqwest client (HTTP/2, rustls).
- SQLite through rusqlite (bundled), WAL mode, one connection behind a mutex. Migrations use
  `PRAGMA user_version`. All access goes through typed functions in `cloud/src/db.rs`, tested against an
  in-memory database. No ORM: the queries are a handful of inserts and sums.
- The server keeps no text and no audio, on any path, error paths included. Logs carry ids, sizes and timings
  only.
- Settings, keys included (Volc, LLM), come from one TOML config file, never from the database.

## Accounts

- `plaid-cloud admin invite <name> [--credit N]` creates the user if needed, adds N credit when given and
  prints a one-time code, valid for 7 days.
- The app exchanges the code for a device token (`POST /v1/login`). The token is kept in the OS credential
  store (account `cloudToken`). It does not expire; `admin revoke <device>` ends it.
- The database stores only the SHA-256 of tokens and codes.
- `admin credit <name> <N|-N>` adds or takes credit. `admin users` and `admin usage [YYYY-MM]` cover the rest.
  There is no web admin.
- Names are 1–8 characters (Unicode, trimmed, no control characters), unique ignoring ASCII case, stored as
  SQLite TEXT. Users rename themselves in 设置 › 服务 › Cloud; `admin rename <name> <new>` does the same.

### Tables

| Table | Columns |
|---|---|
| `users` | id, name, disabled, created, board (takes part in 鲸喷榜, default 1) |
| `devices` | id, user_id, token_hash, name, created, last_seen, revoked |
| `invites` | code_hash, user_id, expires, used_at |
| `usage` | one row per request: at, user_id, device_id, kind, client, received_ms, sent_ms, asr_wait_ms, llm_model, llm_in, llm_out, llm_ms, refine, cost_micros, error, chars (characters delivered, never the text) |
| `credits` | one row per top-up or correction: at, user_id, amount_micros (negative takes credit) |

`usage.refine` is one of none, skipped, ok, timeout, error. `usage.error` holds a code (auth, quota, upstream,
bad_request, disconnected), never upstream text.

### Credit

- Each user has a credit balance: the sum of `credits` minus the sum of `usage.cost_micros`. Every request is
  charged its cost.
- Amounts are integer millionths of a credit. The unit's display name is `plaid_core::cloud::CREDIT`, the one
  place to rename it; prices are set in that unit.
- ASR is priced per hour of audio sent to Volc (`sent_ms`, after the server-side gate). The LLM is priced per
  million input and output tokens, from the provider's `usage` field. All prices come from the config file.
- The balance is checked when a request starts; at zero or below it is refused with `quota`, and the device
  shows `Cloud credit 已用完，请联系管理员充值`. It does not fall back to BYOK. A request in progress is never cut
  off, so the request that crosses zero leaves the balance slightly negative.
- Only the admin adds credit (`admin credit`). Credit does not expire.

## Protocol

- `POST /v1/login` `{code, device}` → `{token, name}`, or 401 with the reason as plain text (unknown, used, expired, disabled), which the app shows as is.
- `GET /v1/account` (bearer) → `{name, balance_micros, month_micros, month_ms}`, for the settings page.
- `GET /v1/board?days=N` (bearer, N 1–90, default 7) → `{days, me, joined, rows:[{name, chars, audio_ms, dictations}]}`:
  completed dictations of users who take part, most characters first. A user who left gets `joined: false` and no rows.
- `POST /v1/board` (bearer) `{join}` → 204: leave or rejoin 鲸喷榜.
- `POST /v1/name` (bearer) `{name}` → 204, 400 with the reason as plain text (empty, too long), or 409 when
  another user has the name.
- `GET /v1/ws` (bearer), websocket. One request per connection in v1. Whether a persistent connection is worth
  having gets measured from `done` timings first.

Client → server:

- `{"type":"start","kind":"dictate","client":"0.11.61","asr":{dialog,hotwords,threshold,nonstream},"refine":Plan|null}`
- Binary frames of 16 kHz mono s16le PCM, any size.
- `{"type":"finish"}`. Closing the socket cancels: the server drops the Volc session and any LLM call in flight.
- `{"type":"start","kind":"refine"|"learn","text":"…","refine":Plan}`: no audio.

Server → client:

- `{"type":"partial","text"}`.
- `{"type":"final","text","refining"}`. The text has had tidy and the hotword rules applied, as the BYOK
  session does.
- `{"type":"refined","text"}` or `{"type":"refine_error","message","timed_out"}`, only when `refining` was true.
- `{"type":"done","usage":{received_ms,sent_ms,asr_wait_ms,llm_ms,llm_in,llm_out,cost_micros,balance_micros}}`; the balance is after this request.
- `{"type":"error","code","message"}`, where code is one of auth, quota, upstream, bad_request.

### The refine plan

`refine::Plan` is built on the device from settings, history and dictionary, and the device and the server
decide with the same pure function (`Plan::decide`).

- Fields: level, local rules, min chars, system prompt (styles appended), the user message rendered with every
  placeholder except `{{text}}`, the voice-command triggers and wake word, and the timeout.
- Command `run` lines stay on the device; only the triggers go up, so the server can tell a command from
  dictation.
- Steps, each run once:
  1. Tidy and the hotword rules: in the Volc session on the server, or on the device for BYOK.
  2. Local rules: decided identically on both sides.
  3. The LLM: on the server.
  4. Post-LLM `format`: on the device.
- For BYOK the device builds the same plan after the final and sends the prompt to its own endpoint, so the
  prompt each mode produces is identical (tested).

### Fail-open

- The device waits for `refined` at most the plan's timeout + 1.5 s. A server that goes silent, a dropped
  socket and a `refine_error` all paste the raw text, as BYOK does on an LLM failure.
- Esc or a new press during refine drops the socket, which aborts the server's LLM call.

### Server-side gate

The server runs the same silence gate over what it receives, re-chunked into 100 ms buffers, so the billed
`sent_ms` is decided by the server. The device gates too, which only saves upload. A second gate over gated
audio sends nearly all of it: every remaining pause is under the keep window.

### Usage on every exit

A usage row is written for every request that reached Volc or the LLM, including cancels, client disconnects
and upstream errors, because Volc bills what it received.

## Client

- `serviceMode` = `byok` (default) or `cloud`. The server URL is built in (`https://plaid.rsod.top`, `config::cloud_url`);
  `PLAID_CLOUD_URL` at run time overrides it, for tests.
- 设置 › 服务 › Cloud has the invite code and 登录, then the account, the balance, this month's
  spend and minutes, 使用 Cloud and 退出登录.
- In cloud mode, refine, 重新润色 and learning go through the server; the LLM endpoint settings are not used.
  Retry from history (重新识别) uses a cloud `dictate` with no plan.

## LLM processing and latency

Baseline (BYOK, Cerebras `qwen-3.8-27b`, 118 refined dictations): release→final p50 739 / p90 1117 ms, LLM
p50 508 / p90 1254 ms, total p50 1373 / p90 2069 ms. LLM time barely depends on length, so most of it is fixed
cost: round trips, connection setup, queueing and prefill.

In v1:

1. **One warm HTTP/2 connection per provider.** It opens at startup and is kept by PING while idle
   (`http2_keep_alive_while_idle`), with no idle timeout. Concurrent requests multiplex on it. A request is one
   round trip, with no TLS handshake. Like the device, the server also warms when a dictation with a plan starts,
   so a dropped connection is reopened while the user speaks.
2. **Stable prefix first.** The system prompt and styles go first, then the user message in template order:
   context, dictionary, corrections, app, history, and the text last. Providers with prefix caching skip the
   repeated part.
3. **The timeout comes from the plan** (the device's `llmTimeoutMs`, capped by the server), failing open to
   the raw text.
4. **Timings in `done` and `usage`:** time from finish to the final result, LLM time, and tokens.

Later, each one only after the data says it pays:

- **Speculative refine on the last partial at finish.** The result is kept if the final matches, otherwise
  the call is cancelled and rerun. Log the partial == final rate first.
- **A hedged request around p75 (~700 ms),** to the same or another provider; the first answer wins.
- **Provider and model per level,** chosen by an eval harness replaying the corpus through Cloud and measuring
  first-token time, total time and edit quality. Placement matters: from a Hong Kong host Volc is ~40 ms away
  and Cerebras (US) ~150–200 ms.

Not planned:

- Streaming refine: the paste waits for the full text anyway.
- Per-segment refine during speech: texts are short (p90 126 chars).
- Diff output: output speed is not the bottleneck.

## Changes from the design as first agreed

- Two small HTTP endpoints sit beside the websocket: login, and account usage for the settings page. Neither is
  on the dictation path.
- The cost unit is a millionth of a credit instead of 厘, and a credit balance replaces the monthly budget.
- `start` carries a prompt that is already rendered (`Plan`) instead of the raw level, style and context
  settings. The server needs no settings logic, and the prompt cannot drift between modes.
- v1 uses the device's timeout instead of a fixed 2.5 s server budget, until there is data from Cloud.

- A quota refusal writes no usage row: nothing reached Volc or the LLM.
- When the server skips the refine (no LLM configured, or the plan decides so), the client records it as a
  refine that did not run and delivers the raw text.
- The `refine` and `learn` kinds send the plan's own text through `Plan::user_with`; the server runs no
  decision for them.
- A failed refine reaches the device as a short reason (`LLM 超时`, `LLM 服务出错（HTTP 401）`), never the
  provider's body.
- Cloud mode with no stored token shows `Cloud 未登录` in the footer.
- The device sends its computer name at login, so `admin devices` can tell devices apart.

## Running it

All settings are in one TOML file, `plaid-cloud.toml` in the working directory or `--config FILE`. The
annotated template is `app/cloud/plaid-cloud.example.toml`. Only `[volc] key` is required. Without `[llm]`,
dictations are not refined. Without `[prices]`, credit is never spent. Unknown keys are an error. The file holds
keys, so keep it `chmod 600`; the server warns at startup when others can read it. `db` is relative to the
file's directory.

```
plaid-cloud --config /etc/plaid-cloud/plaid-cloud.toml serve
plaid-cloud --config /etc/plaid-cloud/plaid-cloud.toml admin invite <name> [--credit N]
plaid-cloud --config /etc/plaid-cloud/plaid-cloud.toml admin credit <name> <N|-N>
```

`llm.proxy` carries every server→LLM request: refine, learn and the warm-up. Volc stays direct. With
`socks5h` the proxy resolves the LLM host; with `socks5` the server does. An invalid value stops the server at
startup; the log shows only `scheme://host:port`, never credentials.

The Volc session comes from `plaid-core`, which uses native-tls, so a Linux build links OpenSSL. TLS
termination for the public endpoint sits in front (Caddy or similar); the server itself speaks plain HTTP.

Measured locally with the real Volc key and Cerebras `qwen-3.8-27b` (reasoning off, through a local proxy):
LLM 390–855 ms per refine on the warm HTTP/2 connection, asr_wait 85–790 ms.

## Order

1. Extract the refine decision and prompt into `core/src/refine.rs`.
2. Build the `plaid-cloud` skeleton: database, admin, login, websocket auth, ASR-only dictate.
3. Move the LLM into the server: warm pool, timeout, usage and credit.
4. Add the client: cloud session, settings, login.
5. Add the `refine` and `learn` kinds.
6. Deploy and invite one or two friends; then the eval harness and the deferred items above.
