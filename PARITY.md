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
- Session history: newest-first list with message/tool counts and age, search,
  pin/unpin, archive, delete, and read-only transcript hydration from
  `/api/sessions/{id}/messages` (the rail swaps between bots and sessions; the
  gateway exposes no resume).
- Skills page: searchable installed catalog with category/usage metadata and
  On/Off toggles through `PUT /api/skills/toggle`.
- MCP page: host-level server list with transport/source/endpoint/auth, a
  host-side reachability test, and On/Off per server.
- Memory page over `GET /api/memory`: active provider, discovered backends with
  their availability/configuration status, built-in file sizes, and activation
  for providers the host marks ready (`PUT /api/memory/provider`).
- Skills hub: search across the host's configured sources, read the actual
  SKILL.md and file manifest, run the installer's security scan, and install by
  identifier only after a visible confirmation.

## Gaps, in the order worth closing

### 1. Sessions and projects — **Partly shipped** (unblocked 2026-11-09)

The pod's `state.db` was rebuilt with SQLite `.recover` after btree page
corruption; `PRAGMA integrity_check` is `ok`, 1,843 sessions and 50,357
messages survived (23 unreadable messages lost), and `GET /api/sessions`
returns 200 again. The original is archived off-cluster and a Longhorn
snapshot (`pre-repair`) was taken first.

Shipped: recency list, search, delete, transcript hydration, and pin/archive
through `PATCH /api/sessions/{id}`. Still needed: resume the gateway runtime for
an old id, inline rename, and a project/workspace filter.

### 2. MCP toolsets — **Partly shipped**

Shipped: the host-level list (transport, source, endpoint, auth), host-side
reachability test and per-server enabled toggle. Still needed: add/edit/
delete servers through a validated config buffer, OAuth hand-off, and the
catalog installer.

### 3. Skills hub — **Shipped**

`GET /api/skills/hub/search`, `/preview`, `/scan` and `POST /hub/install` now
back a Browse-hub tab: server-side search, the full SKILL.md and manifest,
the same security verdict the installer uses, and a confirmed install by
identifier. Uninstall/update remain CLI work.

### 4. Memory — **Partly shipped**

Shipped: the active provider, the discovery list with availability and
configuration status, built-in file sizes, and activation for ready providers.
Still needed: generic config forms from each provider's declared schema and a
confirmed reset. Reset is destructive, so it stays out until that flow exists.

### 5. MoA presets — **Later**

`GET /api/model/moa` and `PUT /api/model/moa` exist, but the payload validation
is strict (half-filled slots are rejected with 422) and an editor is a sizeable
surface. Build it after the single-model flow settles.

### 6. Voice — **Partly shipped**

Shipped: `▷ Speak` synthesizes the last assistant reply through the existing
`voice-models` service and plays the WAV via PipeWire/Pulse. Between
Superwhisper's `Alt+Space` dictation and the client's composer, the everyday
speak/listen loop is covered without bundling models.

Still needed: in-app microphone capture, streaming TTS playback, voice
selection UI, and routing STT through the host instead of the desktop's
dictation stack.

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
2. **M2 — host surfaces:** toolsets, skills, memory (read list shipped; config next).
3. **M3 — Omarchy:** theme sync, `--toggle`, optional bar widget.
4. **M4 — breadth:** MoA, voice, cron/Kanban, provider OAuth, artifacts.

## Constraints

- No live trades or secret mutations without explicit instruction; config writes
  only through the same endpoints the Electron app uses.
- QA runs go to a hidden `special:hermes-qa` workspace; no test windows on the
  active desktop. `sway`/`weston` are not installed, so headless capture would
  need a system package decision.
- Never stage multi-GB database copies in `/tmp`: it is an 8 GiB tmpfs, and a
  6.3 GB recovery dump filled it once. Use `/var/tmp` or
  `/var/tmp/hermes-db-repair` (disk-backed). `/etc/tmpfiles.d/tmp.conf` already ages
  `/tmp` out after 3 days and `/var/tmp` after 14.
