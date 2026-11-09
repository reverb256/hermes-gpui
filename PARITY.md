# Parity plan: Grok Bot and Hermes Desktop

Status legend: **Shipped**, **Next**, **Blocked** (waiting on the pod), **Later**.

## Shipped

- Streaming chat, reasoning lines, tool activity, reconnect with the gateway.
- Approval and clarify cards, @-mentions, attachments, live password login.
- Profile rail with derived looks, previews, search; per-bot inspector (Look,
  Connectors, Secrets) over `profiles.*`; vault over `/api/env`.
- Model picker (`/api/model/options`, `/api/model/set`), auxiliary task pins
  (`/api/model/auxiliary`), reasoning effort (`/api/env`-adjacent partial
  `PUT /api/config`), usage and billing (`/api/analytics/usage`,
  `/api/model/info`, `/api/portal`).
- Full-height rail/inspector split with a centre-column composer, Hermes-style
  footer status strip, tray presence.

## Gaps, in the order worth closing

### 1. Sessions and projects — **Blocked**

`GET /api/sessions` returns 503 on the pod (`state.db corrupt`,
`/opt/data/state.db`). Everything else here is cosmetic until history works.
Needed: the pod-side repair, then a session list grouped by recency, resume,
rename, delete, and a project/workspace filter. No client can work around a
corrupt database.

### 2. MCP toolsets — **Next**

Surface already exists in the app under the bot inspector's Connectors tab, but
is read-only per bot. Add a host-level Toolsets page: list servers with
transport/command/url/enabled, toggle enabled, and edit `mcp.json` through a
validated text buffer before saving. Verification path: `/api/mcp/*` plus a
config round trip; no live server changes without an explicit toggle.

### 3. Skills hub — **Next**

`GET /api/skills`, `/api/skills/content`, `PUT /api/skills/toggle`,
`POST /api/skills/hub/install`, `GET /api/skills/hub/preview` are all available.
Add a browser with search, content preview and enable/disable; installations are
mutations and need a confirm step.

### 4. Memory — **Next**

Memory providers have declared config surfaces the desktop already uses. Add a
read-only memory list plus provider settings; reset stays behind a confirm.

### 5. MoA presets — **Later**

`GET /api/model/moa` and `PUT /api/model/moa` exist, but the payload validation
is strict (half-filled slots are rejected with 422) and an editor is a sizeable
surface. Build it after the single-model flow settles.

### 6. Voice — **Later**

TTS/STT and audio capture need platform work (PipeWire capture, playback) that
GPUI does not give us for free. Keep it out until the text client is complete.

### 7. Cron and Kanban — **Later**

Both are substantial pages (`/api/cron/jobs`, Kanban boards). Read-only lists
first, mutations behind confirms.

### 8. Provider setup and OAuth — **Later**

Provider rows already report `authenticated`; add an activation flow that
mirrors the Electron app (API-key form inline, OAuth hand-off). Requires a
polling flow and a local callback surface.

### 9. Artifacts, media, share — **Later**

Inline images, audio and file previews in the transcript; a canvas/artifact
pane; export and share links. This is Grok's biggest remaining visual gap.

### 10. Omarchy integration — **Next**

See the architecture note in the README: read the active theme's `colors.toml`,
add `--toggle` IPC and a Hyprland keybind, then an optional Quickshell bar
widget that summons the window. No QML rewrite.

## Sequencing

1. **M1 — history:** repair `state.db`, sessions list, resume, projects.
2. **M2 — host surfaces:** toolsets, skills, memory (all read-first).
3. **M3 — Omarchy:** theme sync, `--toggle`, optional bar widget.
4. **M4 — breadth:** MoA, voice, cron/Kanban, provider OAuth, artifacts.

## Constraints

- No live trades or secret mutations without explicit instruction; config writes
  only through the same endpoints the Electron app uses.
- QA runs go to a hidden `special:hermes-qa` workspace; no test windows on the
  active desktop. `sway`/`weston` are not installed, so headless capture would
  need a system package decision.
