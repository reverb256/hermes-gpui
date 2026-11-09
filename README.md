# hermes-gpui

A native GPUI desktop client for a Hermes host — no Electron, no Chromium.

It talks to the same two surfaces the official desktop app does:

| Surface | Endpoint | Used for |
| --- | --- | --- |
| REST | `GET /api/profiles`, `/api/profiles/active`, `/api/mcp/servers`, `/api/cron/jobs`, `/api/status` | profile roster, tool inventory, cron, host telemetry |
| JSON-RPC over WebSocket | `ws(s)://host:9119/api/ws` | sessions, `prompt.submit`, streaming `message.delta`, approvals |

The RPC layer is a Rust port of the gateway client Hermes ships in
`hermes-agent/apps/shared/src/json-rpc-channel.ts`: request ids, per-call
timeouts, `gateway.ping` keepalive with a 45 s liveness deadline,
`client.capabilities` advertisement, and replay of server→client requests.

## Layout

```
crates/hermes-rpc        JSON-RPC 2.0 WebSocket channel (transport only)
crates/hermes-api        host config, REST client, session/prompt helpers
crates/hermes-desktop    GPUI app: sidebar, transcript, composer, tray
```

## Running

```bash
# local dashboard on the default port
cargo run -p hermes-desktop

# or point at any host
HERMES_URL=https://hermes.example.com \
HERMES_BASIC=user:pass \
cargo run -p hermes-desktop
```

Env vars: `HERMES_URL` (origin), `HERMES_TOKEN` (bearer), `HERMES_BASIC`
(`user:pass`), `HERMES_PROFILE` (profile to scope the chat to).

## System requirements (Linux)

GPUI renders through the GPU: `wayland` or `x11`, `libxkbcommon`, `vulkan-icd-loader`,
`fontconfig`, `freetype2`. On Arch: `pacman -S base-devel wayland libxkbcommon
vulkan-icd-loader vulkan-headers fontconfig freetype2`.

## Status

Working today: connect/handshake/keepalive, profile roster, active-profile
display, streaming chat transcript with reasoning deltas, prompt submission,
interrupt, tray icon with show/quit.

Not yet wired: approvals and clarify cards (requests are received and surfaced
as system lines), file browser, voice mode, multi-host switching. The composer
handles printable keys, backspace and enter; there is no IME composition or
text selection yet.
