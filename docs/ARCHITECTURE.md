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
└─────────────────────────────┘

┌─────────────────────────────┐
│ hermes-hub (this repo)      │  one owner, SQLite, k3s/OCI
│  crates/hermes-hub          │  enrollment, registry, command router,
│  crates/hermes-protocol     │  audit and secret-prompt broker
└──────────────▲──────────────┘
               │ outbound WSS /v1/node (node dials hub)
┌──────────────┴──────────────┐
│ hermes-node on each computer│  shell now; PTY/files/screen later
│  crates/hermes-node         │  local policy, execution and streams
└─────────────────────────────┘
```

The hub is not a replacement for `hermes-agent`. It is the personal control
plane that connects the computers and gives the GPUI client a stable roster and
audit. A plain host still works without it.

The client must keep working against a plain `hermes-agent` host with no control
plane; everything in the third layer is additive and feature-detected.

## Crates

| Crate | Responsibility | Depends on |
| --- | --- | --- |
| `hermes-rpc` | transport only: request ids, per-call timeouts, keepalive (`gateway.ping`, 45 s deadline), server→client request replay, credential redaction in debug output | tokio-tungstenite, serde |
| `hermes-api` | `HostConfig` (env, URL normalisation, WS URL), REST client, typed payloads, session/prompt helpers, Kanban/plugin clients, local contract tests | reqwest, serde |
| `hermes-protocol` | versioned node/device frames, commands, stream events, prompts and validation; no I/O | serde, thiserror |
| `hermes-hub` | axum API, SQLite registry, one-time enrollment, node/device WebSockets, idempotent routing, audit and prompt broker | axum, rusqlite, hermes-protocol |
| `hermes-node` | outbound agent, local policy, shell execution, output streams and prompt cards | hermes-protocol, tokio |
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

## Personal hub

`hermes-hub` exposes the owner API and two WebSocket surfaces:

| Surface | Endpoint | Auth | Purpose |
| --- | --- | --- | --- |
| owner API | `/api/v1/*` | admin bearer | enroll, list/revoke nodes and devices, submit commands, read audit and prompt metadata |
| node socket | `/v1/node` | node bearer | hello, heartbeat, streams, results, prompt requests |
| device socket | `/v1/device` | device bearer | node list, command submission, streams, prompt cards |

SQLite stores principals (token hashes only), nodes, commands, command events,
prompt metadata and audit. It deliberately does not store command env or a
prompt value. There is one replica because SQLite has one writer.

Secrets never become a command field. The node raises a one-time challenge; the
hub forwards it to every connected device and accepts one matching answer; the
value is forwarded to the node, injected into the child environment, and
zeroized. Audit records the prompt id, scope and outcome, never the value. With
no device connected the hub cancels rather than queueing a credential.

Deployment is a Helm chart in `charts/hermes-hub`, synced by the ArgoCD
Application in `deploy/argocd/`. The chart carries a Tailscale Ingress and an
optional direct-LAN Connector. Images are `docker/hermes-hub.Dockerfile` and
`docker/hermes-node.Dockerfile`; compose is the portable fallback.

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
6. Hub access is separate: the owner holds the admin token, each computer has a
   node token and each UI device a device token. Enrollment codes are single-use
   and expire; long-lived tokens are stored hashed and revoked server-side.
   Tailscale identity is a network gate, not an application credential.

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

A remote command takes the same shape through the hub:

```
device → SubmitCommand → hub command row (idempotency key)
       → /v1/node command → local policy → child process
       ← CommandStarted / Stream(seq) / CommandResult
       ← device CommandState / CommandStream
```

A node disconnect fails in-flight commands and broadcasts the terminal state;
the hub never invents a result. Device reconnects replay `command_events` by
sequence over the REST API.

## State ownership

| State | Owner | Persistence |
| --- | --- | --- |
| Transcript, drafts, open panels | client | memory only |
| Selected Kanban board | client | memory (host active board untouched) |
| Sessions, profiles, secrets, skills, MCP, cron, memory | host | host files/SQLite |
| Hub principals, node registry, commands, events, prompt metadata, audit | hub | hub SQLite volume |
| Pending prompt values | device/node memory only | never persisted |
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
| Hub offline | node reconnects with bounded backoff; device socket fails visibly and commands are not queued |
| Node offline | hub returns 409, fails the queued command and broadcasts the terminal state |
| Prompt unanswered | challenge expires, hub broadcasts `prompt_closed`, node denies the command |
| Prompt value submitted twice | first answer resolves the prompt; later answers are rejected |
| Hub restarted | nodes reconnect; SQLite restores registry, audit and command history |

## Security boundaries

- Secret values are written but never read back: the reveal endpoint is not
  called; lists show redacted values.
- Markdown never fetches remote content; host-local images load only on explicit
  action.
- Kanban attachment downloads stream to a user-chosen path; uploads are capped
  and named by the host.
- Debug output redacts tokens, cookies and query strings. `PromptDecision`
  redacts its value in `Debug`.
- Hub tokens are 256-bit random values stored as SHA-256 hashes; admin
  comparison is constant-time. Node/device principals are scoped by kind and id.
- The node clears its child environment, so `HERMES_NODE_TOKEN` and host secrets
  are not inherited. Prompted secrets enter only through the named env var.
- The host and hub are single-user by construction; there is no tenant or role
  model to misconfigure. Multi-user work is out of scope, not deferred.
