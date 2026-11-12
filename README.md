# hermes-gpui

[![CI](https://github.com/reverb256/hermes-gpui/actions/workflows/ci.yml/badge.svg)](https://github.com/reverb256/hermes-gpui/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A native GPUI desktop client for a Hermes host — no Electron, no Chromium.

In this fleet the host is the **`hermes-crew` pod** (namespace `hermes-crew`):
one gateway process serving every profile, with profile state on the Hermes PVC
and memory in the in-cluster OpenRecall store. This client is the frontend for
that pod; it does not run its own gateway.

## Running against hermes-crew

```bash
source scripts/hermes-crew-env.sh   # HERMES_URL + HERMES_BASIC
cargo run -p hermes-desktop
```

The helper defaults to the Service VIP `http://10.43.29.77:9119`, which is
correct on a host that is itself a k3s node. Set `HERMES_CREW_URL` for anything
else — the tailnet ingress is `https://hermes-crew.tigris-ule.ts.net`, subject
to the tailnet ACL. Credentials come from `HERMES_CREW_USER`/`HERMES_CREW_PASS`
or the `hermes-dashboard` Secret; nothing is written to disk.

The helper exports `HERMES_TOKEN` when it can read the deployment session token
from BSM (key `hermes-crew-session-token`, needs `BWS_ACCESS_TOKEN`) **and** the
gateway accepts it — that is the preferred path, since a bearer is also the
`?token=` the WebSocket uses and it survives gateway restarts. Otherwise it falls
back to `HERMES_BASIC`, and the client POSTs `/auth/password-login` (provider
`basic`) itself and keeps the session cookie.

The token only works once the gateway has been started with
`HERMES_DASHBOARD_SESSION_TOKEN` set to the same value; without it the server
mints a random token per start. As of 2026-11-12 the value is in BSM but the
cluster's Secret is not carrying it yet, because External Secrets is getting a
400 from the Bitwarden API when it lists organization secrets. So the login path
is what works today.

It talks to the same two surfaces the official desktop app does:

| Surface | Endpoint | Used for |
| --- | --- | --- |
| REST | `GET /api/profiles`, `/api/profiles/active`, `/api/mcp/servers`, `/api/cron/jobs`, `/api/status`, `GET/PUT/DELETE /api/env` | profile roster, tool inventory, cron, host telemetry, secrets |
| JSON-RPC over WebSocket | `ws(s)://host:9119/api/ws` | sessions, `prompt.submit`, streaming `message.delta`, approvals |

The RPC layer is a Rust port of the gateway client Hermes ships in
`hermes-agent/apps/shared/src/json-rpc-channel.ts`: request ids, per-call
timeouts, `gateway.ping` keepalive with a 45 s liveness deadline,
`client.capabilities` advertisement, and replay of server→client requests.

## Documents

- [`ROADMAP.md`](ROADMAP.md) — phases, workstreams, milestones and status.
- [`PARITY.md`](PARITY.md) — per-feature status against Hermes Desktop and Grok Bot.
- [`docs/FULL-PARITY-PLAN.md`](docs/FULL-PARITY-PLAN.md) — the single-user build plan (personal hub, node agents, connectors, sync, artifacts).
- [`docs/README.md`](docs/README.md) — full documentation index.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md), [`docs/FEATURES.md`](docs/FEATURES.md) — how it is built and what each surface does.
- [`docs/NODE-PROTOCOL.md`](docs/NODE-PROTOCOL.md) — hub/node/device wire contract, pairing, prompts and failure semantics.
- [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md), [`docs/OPERATIONS.md`](docs/OPERATIONS.md) — build/test and run/operate.
- [`docs/OMARCHY.md`](docs/OMARCHY.md) — Omarchy integration (theme, notifications, bar widget, Superwhisper).
- [`docs/adr/`](docs/adr/README.md) — architecture decision records.

## Layout

```
crates/hermes-rpc        JSON-RPC 2.0 WebSocket channel (transport only)
crates/hermes-api        host config, REST client, session/prompt helpers
crates/hermes-protocol   versioned hub/node/device frames and validation
crates/hermes-hub        owner API, SQLite registry, node/device sockets, audit
crates/hermes-node       outbound agent: local policy, shell execution, prompts
crates/hermes-desktop    GPUI app: sidebar, transcript, composer, tray
charts/hermes-hub        Helm chart (Deployment, PVC, Tailscale Ingress)
deploy/argocd            ArgoCD Application
docker, deploy/compose   OCI images and portable fallback
```

## Personal hub

The hub is the single-owner control plane for reaching several computers from
one client: each computer runs `hermes-node`, dials out to `hermes-hub` and
advertises what it can do. No inbound port is needed on a node, and secret
prompts are brokered as one-time cards rather than stored credentials. The
protocol is [`docs/NODE-PROTOCOL.md`](docs/NODE-PROTOCOL.md).

```bash
export HERMES_HUB_ADMIN_TOKEN="$(openssl rand -base64 48)"
cargo run -p hermes-hub    # http://127.0.0.1:8080

# on each computer, with a token from POST /api/v1/enroll
HERMES_HUB_URL=http://127.0.0.1:8080 \
HERMES_NODE_ID=node-… HERMES_NODE_TOKEN=… \
cargo run -p hermes-node

# on the client, with a token from a "device" enrollment
HERMES_HUB_URL=http://127.0.0.1:8080 \
HERMES_HUB_DEVICE_ID=device-… HERMES_HUB_DEVICE_TOKEN=… \
cargo run -p hermes-desktop
```

The hub deploys from `charts/hermes-hub` through the ArgoCD Application in
`deploy/argocd/hermes-hub.yaml`, behind a Tailscale Ingress. Compose is in
`deploy/compose`. The GPUI client connects to `/v1/device`: `Ctrl+Shift+H` or the
**Computers** header button lists enrolled nodes, runs argv-only commands, opens
a terminal on nodes that advertise `pty`, and shows one-time credential cards.
`HERMES_OPEN_SETTINGS=computers` opens the sheet at launch for QA.

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
login), `HERMES_PROFILE` (profile to scope the chat to), and the optional voice
pair `HERMES_TTS_URL` / `HERMES_TTS_VOICE`.

### Voice

`▷ Speak` in the header reads the last assistant answer through the stack's
`voice-models` service (`POST /tts`, Chatterbox Turbo by default) and plays the
returned WAV with the first of `pw-play`, `paplay` or `aplay` on `PATH`. Point it
at the service in one of the usual ways:

```bash
# LAN NodePort (use your node address)
HERMES_TTS_URL=http://NODE_IP:30805 cargo run -p hermes-desktop

# or a local port-forward
kubectl -n voice-models port-forward svc/voice-models 8004:8004 &
HERMES_TTS_URL=http://127.0.0.1:8004 cargo run -p hermes-desktop
```

`HERMES_TTS_VOICE` defaults to `en-US-AndrewNeural`. The desktop does not bundle
a TTS model or capture audio. Dictation stays with Superwhisper: `Alt+Space`
records, and its fcitx5 text commit pastes into the focused composer.

### Omarchy theme

On Omarchy the palette comes from the active theme at
`~/.local/state/omarchy/current/theme/colors.toml`. Background, foreground,
accent, selection, muted and the named ANSI colours fill the semantic tokens;
missing keys fall back to the bundled palette. The file is re-read at most every
two seconds, so `omarchy theme set …` is reflected without restarting the app.
There is no separate client theme: the palette is Omarchy's, and a missing
`colors.toml` falls back to the bundled palette.

### Omarchy launcher

`hermes-desktop --toggle` shows or hides the existing instance; a normal second
launch brings it forward. The tray also offers **Show Hermes** and **Quit**.
For example, add a binding to your own Hyprland configuration after choosing
an unused shortcut (and ensure `hermes-desktop` is on `PATH`):

```ini
bind = SUPER, H, exec, hermes-desktop --toggle
```

The per-user socket defaults to `$XDG_RUNTIME_DIR/hermes-gpui-$USER.sock`, or
the temporary directory if `XDG_RUNTIME_DIR` is unset. `HERMES_GPUI_SOCKET`
overrides the path. This example does not modify your desktop configuration.

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


### Pointing at a remote deployment

A remote k3s deployment serves the same dashboard and gateway, so no special mode is needed:

```bash
HERMES_URL=https://hermes.example.com \
HERMES_BASIC=user:pass \
HERMES_PROFILE=ops \
cargo run -p hermes-desktop
```

The roster comes from the gateway's `profiles.list` (falling back to
`/api/profiles`), so every profile shows up in the sidebar with its own last
message as a preview.

## Style

The UI is built on a token layer (`crates/hermes-desktop/src/theme.rs`) that
applies the [GPUI Kit style guide](https://gpui-kit.com/docs/style): tokens
before values, semantic colours, one accent, a 4 px spacing rhythm and one
reading measure. [`STYLE.md`](STYLE.md) records the rules and token table;
contrast and scale invariants are unit-tested.

## Bot inspector

`Ctrl+,` (or the `⚙ Bot` button) opens a panel for the selected bot:

| Tab | What it does | Wire surface |
| --- | --- | --- |
| Look | icon glyph, accent colour and avatar image, shared by every client | `profiles.configure` (`ui_meta.icon`, `ui_meta.accent`), `profiles.set_asset` / `profiles.get_asset` |
| Connectors | switch MCP servers and toolsets on or off | `profiles.describe`, `profiles.configure` (`enabled_mcp_servers`, `enabled_toolsets`) |
| Secrets | list, add, replace and delete host env keys, always redacted | `GET/PUT/DELETE /api/env` |

Look changes live in the host's `profile.yaml`, so a bot keeps its face on every
machine. Avatar images are stored as `assets/avatar.*`; the client accepts
PNG/JPEG/WebP up to 2 MB. Secret values arrive redacted and the reveal endpoint
is never called, so the client can write a secret but cannot read one back.

## Settings

`Ctrl+Shift+V` (Vault), `Ctrl+Shift+M` (Model), `Ctrl+Shift+A` (Assignments),
`Ctrl+Shift+K` (Skills), `Ctrl+Shift+X` (MCP), `Ctrl+Shift+S` (Sessions) and
`Ctrl+Shift+U` (Usage & Billing) open their surfaces; the settings sheet
keeps its sections down the left edge. `Vault ⇗` on the inspector's Secrets tab opens the same sheet on
Vault.

**Vault** is the host's credential catalog, grouped into one tab per category the
host reports, with set counts, search and a per-entry replace form. `New item`
writes a key through `PUT /api/env`. Rows show the redacted value; the reveal
endpoint is never called, so the sheet can write a credential but not read one
back.

**Model** ports the Electron model picker: `/api/model/options` lists the
authenticated providers and their curated model ids, searchable by provider or
model, and picking one writes `POST /api/model/set`. That applies to new
sessions only — an open chat keeps the model it started with — so the page says
so instead of implying a hot swap.

**Assignments** ports the two config-backed knobs the Electron app keeps beside
the model picker: `GET /api/model/auxiliary` lists the host's helper tasks
(vision, compression, titles, review, curation and friends — the list is
server-driven) with their pinned `provider · model` or `Follows main`; picking one reuses the
model catalog and posts `scope: auxiliary` to `/api/model/set`. `Reset all to
main` uses the backend's `__reset__` sentinel. The same page carries the
reasoning scale (default/minimal/low/medium/high/xhigh/max/ultra), read from
`agent.reasoning_effort` in `/api/config` and written back with a partial deep
merge. Both apply to new sessions only.

**Sessions** swaps the bot rail for history: `/api/sessions` newest-first with
message/tool counts and relative age, a client-side search, inline rename, pin/archive, and delete per row.
Opening one requests `session.resume`; older hosts fall back to a read-only
transcript. Filter the loaded list by project, or export a session as JSON using
the download button. `Ctrl+Shift+S` or the header's **Sessions** chip toggles
the rail; Escape/Bots goes back.

Standalone Markdown image references to host-local PNG, JPEG or WebP files in
assistant messages offer **Load image**, which uses the host's restricted media
endpoint. Remote URLs are not loaded automatically.

**MoA** edits existing mixture-of-agents presets, keeping the full preset map
when saving. **Automations** lists cron jobs and supports creation, pause,
resume, trigger, deletion and run history. **Accounts** shows provider OAuth
status and supports host-managed sign-in and disconnect where available.

**MCP** lists configured servers with transport, source, endpoint and auth,
a host-side **Test** ping (`POST /api/mcp/servers/{name}/test`) and an On/Off
toggle (`PUT /api/mcp/servers/{name}/enabled`). **Add server** opens a validated
HTTP/stdio form; **Edit** reads the raw entry and saves through the host's
whole-map `PUT /api/mcp/servers`, preserving tool selection and unknown keys.
**Delete** requires a second click. The **Catalog** tab installs curated
entries, prompts for required env vars, and reports clone/build jobs. HTTP
servers offer a host-managed **Authorize** flow with browser hand-off and cancel;
this flow has not yet been verified against a live OAuth provider.

**Kanban** reads boards from the optional Hermes Kanban plugin. Board selection
is local and requests are scoped to the selected board. It supports basic,
host-validated status transitions, title-only triage task creation and editing,
and two-click task deletion. Boards can be created and their display names edited
without changing the host's active board. Task details show descriptions, summaries,
results, comments and attachments; comments can be added and attachments
uploaded, saved and removed, and a task's description, assignee and priority can be edited.
Boards can be renamed, archived or deleted with confirmation. Other board
settings and drag-and-drop are not yet implemented.

**Memory** lists discovered providers, opens a schema-driven **Configure** form
for every declared provider (text, secret, boolean, number, select and JSON
fields, including conditional fields), and saves through
`PUT /api/memory/providers/{name}/config`. Built-in memory files have separate
reset buttons; each requires a second confirming click before
`POST /api/memory/reset`.

**Skills** lists the host's installed skills with search, category pills,
usage counts and a live On/Off toggle (`PUT /api/skills/toggle`); the footer
says changes apply to new turns rather than mid-answer. **Browse hub** searches
the host's configured sources, previews the actual `SKILL.md`, runs the
installer's security scan and installs an identifier only after confirmation.

A Hermes-style footer strip runs under every pane (they all end on the same
line): gateway health on the left, the current model and crate version on the
right.

**Usage & Billing** reads `/api/analytics/usage` for the 7/30/90-day totals and
per-model spend, `/api/model/info` for the running model and its context, and
`/api/portal` for the account tier and where billing is managed. A host whose
`state.db` is unreadable answers 503, and the page shows that explanation rather
than an empty chart. Billing mutations are a gateway RPC surface, so this page
reports and links out instead of moving money.

`HERMES_OPEN_INSPECTOR=1` opens the panel at launch,
`HERMES_INSPECTOR_TAB=look|connectors|secrets` picks the tab, and
`HERMES_OPEN_SETTINGS=vault|model|assignments|skills|mcp|sessions|usage` opens
the settings sheet on that section or rail.
`HERMES_DEMO=1` seeds a short local transcript. Those are QA hooks for screenshot
runs, which is how the panel, sheet and message styling are reviewed without
input automation.

## Development

Rust is pinned by `rust-toolchain.toml`; rustup installs the toolchain and the
`rustfmt`/`clippy` components automatically. The Linux libraries below are
required to build and run.

```bash
make check   # fmt + clippy + tests, exactly what CI runs
```

CI runs the same three commands on every push and pull request. See
[`CONTRIBUTING.md`](CONTRIBUTING.md) for the PR flow and
[`STYLE.md`](STYLE.md) for UI rules.

## System requirements (Linux)

GPUI renders through the GPU: `wayland` or `x11`, `libxkbcommon`, `vulkan-icd-loader`,
`fontconfig`, `freetype2`. On Arch: `pacman -S base-devel wayland libxkbcommon
vulkan-icd-loader vulkan-headers fontconfig freetype2`.

## Contributing

Bug reports and pull requests are welcome. Read [`CONTRIBUTING.md`](CONTRIBUTING.md)
and [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md); all three CI checks must pass.

## Security

Report vulnerabilities privately through the repository's Security tab as
described in [`SECURITY.md`](SECURITY.md). Never include live credentials or
session cookies in an issue.

## Changelog

Released changes are recorded in [`CHANGELOG.md`](CHANGELOG.md).

## Status

Working today: connect/handshake/keepalive, password login + WS tickets for
gated hosts, profile roster with last-message previews and search, bot switching
(`Ctrl+1…9`), streaming transcript with reasoning deltas, attributed bot cards
and a pill composer, prompt submission, interrupt, approval cards
(allow once / always allow / deny), clarify cards, `@`-mentions for bots and
skills, file and image attachments, cron routines read-out, tray icon with
show/quit and minimise-on-close, a per-bot inspector for look, connectors (MCP
servers and toolsets) and host secrets, session history, installed-skills
toggles, MCP add/edit/delete plus the catalog installer, a memory-provider page
with generic schema-driven configuration and confirmed resets, a scan-gated
skills-hub browser with confirmed
installs, TTS playback of the last reply through `voice-models`, and an Omarchy
theme that follows the live desktop palette.
`Ctrl+F` opens a find bar over the loaded transcript: it highlights matches and
cycles them with Enter / Shift+Enter. Dictation remains Superwhisper's job
(`Alt+Space`), since the client does not capture audio itself.

Working through the personal hub: a node roster with presence, argv-only remote
commands with streamed output, a PTY terminal per computer (keys, resize and
hangup) and masked one-time credential cards. All four computers run the agent.

Not yet wired: file browser, voice mode, file/screen/browser/GPU node
executors, full Markdown formatting, general artifact previews, and advanced
Kanban management. The
composer uses GPUI's native multiline editor for cursor navigation, selection,
undo and IME input; accessibility and platform behavior still need end-to-end
validation. An isolated Hermes 0.20.3 host was used to
verify the gateway probe and Kanban HTTP contracts; provider OAuth and the other
new integrations have not yet been verified end to end.
