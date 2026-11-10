# Architecture

**Last updated:** 2026-11-10.

## Three layers

```
┌─────────────────────────────┐
│ hermes-gpui (this repo)     │  native GPUI client, no Electron
│  crates/hermes-rpc          │  JSON-RPC 2.0 over WebSocket
│  crates/hermes-api          │  REST client, host config, session helpers
│  crates/hermes-desktop      │  app, views, theme, tray, IPC
└──────────────┬──────────────┘
               │ REST + ws(s)://…/api/ws
┌──────────────▼──────────────┐
│ hermes-agent (sibling repo) │  FastAPI host + agent runtime + plugins
│  /api/*  +  JSON-RPC gateway│  sessions, tools, MCP, cron, memory, voice,
│  plugins: kanban, memory, … │  browser, computer_use, PTY, Kanban
└──────────────┬──────────────┘
               │ (planned) events, control-plane JWTs, sandbox APIs
┌──────────────▼──────────────┐
│ hermes-control (to build)   │  Postgres: accounts, orgs, roles, bots,
│  + sandbox fleet on k3s     │  shares, routines, audit, usage, policies
└─────────────────────────────┘
```

The client must keep working against a plain `hermes-agent` host with no control
plane; everything in the third layer is additive and feature-detected.

## Crates

| Crate | Responsibility | Depends on |
| --- | --- | --- |
| `hermes-rpc` | transport only: request ids, per-call timeouts, keepalive (`gateway.ping`, 45 s deadline), server→client request replay, credential redaction in debug output | tokio-tungstenite, serde |
| `hermes-api` | `HostConfig` (env, URL normalisation, WS URL), REST client, typed payloads, session/prompt helpers, Kanban/plugin clients, local contract tests | reqwest, serde |
| `hermes-desktop` | application: `HermesApp` (GPUI entity), backend worker, views, theme, tray, IPC, and the surface modules (`kanban`, `sessions`, `mcp`, `cron`, `memory`, `moa`, `models`, `oauth`, `skills`, `transcript`, `usage`, `vault`, `voice`, `omarchy`, `status`) | gpui-ce, hermes-api, rfd |

## Runtime shape

- `main.rs` builds `HostConfig`, handles `--probe` and `--toggle`, claims the
  single-instance socket, then opens the GPUI window.
- `backend.rs` owns a tokio worker. The UI sends `Command`; the worker performs
  REST/RPC calls and sends `BackendEvent` back over a channel, which the app
  applies on the main thread. No network call happens on the UI thread.
- `app.rs` holds event wiring and window state; surface modules own their own
  state and views (`kanban.rs` holds the board view and its fields). Feature
  helpers stay pure where possible so their rules are unit-tested without a
  window (streaming merge, Markdown spans, session filtering, Kanban state).
- One 200 ms tick drives the tray, the IPC socket, window presence and a 2 s
  theme poll, so their ordering lives in one place.
- Closing the window hides to the tray so a turn keeps streaming; Quit is on the
  tray and in the command palette.

## Wire surfaces used

| Surface | Examples | Notes |
| --- | --- | --- |
| REST | `/api/profiles`, `/api/mcp/servers`, `/api/cron/jobs`, `/api/env`, `/api/memory/*`, `/api/skills/*`, `/api/plugins/kanban/*`, `/api/analytics/usage`, `/api/model/*`, `/api/portal` | bearer token, session cookie, or basic auth depending on host gates |
| JSON-RPC | `session.*`, `prompt.submit`, `message.delta`, approvals, `profiles.*`, `browser.*` | `ws(s)://host/api/ws` |
| Plugin REST | `/api/plugins/kanban/board|tasks|boards|attachments` | optional plugin; every request carries `?board=<slug>` |
| Host media | `/api/media?path=` | restricted to host-local image paths |
| External TTS | `POST {HERMES_TTS_URL}/tts` | playback via `pw-play`/`paplay`/`aplay` |
| IPC | `$XDG_RUNTIME_DIR/hermes-gpui-$USER.sock` (or `HERMES_GPUI_SOCKET`) | `Show`/`Toggle` requests between instances |

## Authentication flow

1. If `HERMES_TOKEN` is set, every request uses bearer auth.
2. Else if `HERMES_COOKIE` is set, that cookie is sent directly.
3. Else if `HERMES_BASIC` is set: `GET /api/auth/providers`, pick the provider
   with `supports_password`, `POST /auth/password-login`, keep
   `__Host-hermes_session_at`.
4. Before every WebSocket dial on a gated host, mint a single-use ticket with
   `POST /api/auth/ws-ticket` and append `?ticket=…` (tickets live 30 s).
5. Loopback token auth for the WS uses `?token=…`; connection debug output
   redacts query strings so credentials never reach logs.

## Data flow: a prompt

```
composer → Command::Submit → worker → prompt.submit (WS)
        ← message.delta / reasoning.delta / tool.* events
        ← transcript lines (streaming merge by role)
        ← approval request → card → Command::Respond → approval RPC
```

Failures are surfaced as status lines and are never replayed silently. Offline
commands map to an explicit `*Failed` event (see the offline test in
`crates/hermes-desktop/src/backend.rs`).

## State ownership

| State | Owner | Persistence |
| --- | --- | --- |
| Transcript, drafts, open panels | client | memory only |
| Selected Kanban board | client | memory (host active board untouched) |
| Sessions, profiles, secrets, skills, MCP, cron, memory | host | host files/SQLite |
| Theme | Omarchy file | `~/.local/state/omarchy/current/theme/colors.toml` |
| Single instance | socket + PID file | runtime dir |

## Failure modes and behaviour

| Failure | Behaviour |
| --- | --- |
| Gateway offline | actions fail visibly with the host's message; nothing queued |
| Host gate requires login | password login + WS ticket; `--probe` reports each step |
| Kanban plugin absent | Kanban surface reports the plugin error instead of an empty board |
| Media path rejected | inline image shows the load error; remote URLs are never fetched |
| TTS service or player missing | header button reports the error; no audio |
| Another instance running | second launch forwards Show/Toggle and exits |
| Socket stale | claimed again; unresponsive primary exits with an error |

## Security boundaries

- Secret values are written but never read back: the reveal endpoint is not
  called; lists show redacted values.
- Markdown never fetches remote content; host-local images load only on explicit
  action.
- Kanban attachment downloads stream to a user-chosen path; uploads are capped
  and named by the host.
- Debug output redacts tokens, cookies and query strings.
- The current host model is single-user; multi-user isolation is platform work
  described in [`FULL-PARITY-PLAN.md`](FULL-PARITY-PLAN.md).
