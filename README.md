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
(`user:pass`), `HERMES_COOKIE` (a ready-made `hermes_session_at=…` cookie, skips
login), `HERMES_PROFILE` (profile to scope the chat to).

### Authentication

A gated dashboard (`auth_required: true`) does not accept basic auth on its
routers or on the socket. When only `HERMES_BASIC` is set the client:

1. `GET /api/auth/providers` → picks the provider with `supports_password`
   (`basic` for `HERMES_DASHBOARD_BASIC_AUTH_*` deployments);
2. `POST /auth/password-login` → keeps the `__Host-hermes_session_at` cookie;
3. sends that cookie on every REST call and mints a fresh single-use
   `POST /api/auth/ws-ticket` before each dial, appending `?ticket=…` to the
   `/api/ws` URL (tickets live 30 s and are consumed by the handshake).

`--probe` performs the whole sequence headlessly and reports each step, which
makes it the quickest way to check a host:

```bash
HERMES_URL=https://hermes.example.com HERMES_BASIC=user:pass   ./target/debug/hermes-desktop --probe
```


### Pointing at the hermes deployment

The k3s pod serves the same dashboard and gateway, so no special mode is needed:

```bash
HERMES_URL=https://hermes.example.com \
HERMES_BASIC=user:pass \
HERMES_PROFILE=ops \
cargo run -p hermes-desktop
```

The profile roster comes from the pod's `/api/profiles`, so every `fleet-*`
profile shows up in the sidebar automatically.

## Style

The UI is built on a token layer (`crates/hermes-desktop/src/theme.rs`) that
applies the [GPUI Kit style guide](https://gpui-kit.com/docs/style): tokens
before values, semantic colours, one accent, a 4 px spacing rhythm and one
reading measure. [`STYLE.md`](STYLE.md) records the rules and token table;
contrast and scale invariants are unit-tested.

## System requirements (Linux)

GPUI renders through the GPU: `wayland` or `x11`, `libxkbcommon`, `vulkan-icd-loader`,
`fontconfig`, `freetype2`. On Arch: `pacman -S base-devel wayland libxkbcommon
vulkan-icd-loader vulkan-headers fontconfig freetype2`.

## Status

Working today: connect/handshake/keepalive, password login + WS tickets for
gated hosts, profile roster, bot switching (`Ctrl+1…9`), streaming transcript
with reasoning deltas, prompt submission, interrupt, approval cards
(allow once / always allow / deny), clarify cards, `@`-mentions for bots and
skills, file and image attachments, cron routines read-out, tray icon with
show/quit and minimise-on-close.

Not yet wired: file browser, voice mode, inline Vault-style secrets,
multi-host switching. The composer handles printable keys, backspace and enter;
there is no IME composition or text selection yet.
