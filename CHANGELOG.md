# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Session resume with a read-only fallback, project filtering and JSON export.
- On-demand inline host-local PNG, JPEG and WebP image previews.
- Single-instance `--toggle` IPC, tray show action and a Hyprland binding example.
- MoA preset editor, cron job controls and provider OAuth accounts page.
- MCP OAuth browser hand-off and Kanban board selection, creation and renaming, task title editing,
  confirmed deletion, status transitions, triage task creation, task details, comments
  and attachments (upload, save and remove), with description, assignee and priority editing,
  plus confirmed board archive and deletion; session history pages beyond the first 50 rows and replies can be copied to the clipboard.
- Streamed Markdown block and inline styling and a GPUI multiline composer with
  selection, editing shortcuts, IME support and an accessible label.
- Readable derived Omarchy theme hints and explicit offline-action errors.
- Larger settings navigation and refreshed session cards using theme tokens.
- Skills-hub browser: source-wide search, SKILL.md and manifest preview, the
  installer's security verdict, and confirmed installs by identifier.
- Voice playback: `▷ Speak` synthesizes the last assistant reply through the
  stack's `voice-models` service and plays it through PipeWire/Pulse.
- Session controls: pin/unpin, archive and inline rename through the host's
  `PATCH /api/sessions/{id}`, with the list refreshed from the host.
- MCP management: validated add/edit/delete through the host's config API,
  preserving unknown keys on edit, plus a catalog browser that prompts for
  required environment variables and reports background clone/build jobs.
- Memory configuration: schema-driven forms for every declared provider,
  including typed/conditional fields and write-only secrets, with a confirmed
  reset for MEMORY.md, USER.md or both.
- Memory page: active provider, discovery status and built-in file sizes from
  `GET /api/memory`, plus activation for providers the host marks ready.
- Omarchy theme sync: the runtime palette follows the active theme's
  `colors.toml`, with derived surfaces, colour fallbacks, and a two-second
  refresh so theme changes apply live.

## [0.1.0] - 2026-11-09

### Added

- Native GPUI desktop client for Hermes: streaming transcript, reasoning lines,
  tool activity and reconnect handling over the gateway WebSocket.
- Password login for gated dashboards, session cookies and single-use WebSocket
  tickets, plus a headless `--probe` path.
- Profile rail with derived looks, previews and search; per-bot inspector for
  look, connectors and redacted host secrets.
- Model picker, auxiliary task assignments and reasoning effort, all scoped to
  new sessions.
- Vault page over the redacted `/api/env` catalog.
- Usage and billing read-outs from `/api/analytics/usage`, `/api/model/info` and
  `/api/portal`.
- Session history with search, delete and read-only stored-transcript
  hydration (the gateway exposes no resume call).
- Installed-skills page with search, metadata and On/Off toggles.
- Host-level MCP page with transport/endpoint/auth detail, reachability test and
  per-server toggle.
- Tray presence, attachments, `@`-mentions, approval/clarify cards and
  Hermes-style footer status strip.
- MIT license, upstream notice for the ported gateway contract, style guide and
  parity plan.

[Unreleased]: https://github.com/reverb256/hermes-gpui/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/reverb256/hermes-gpui/releases/tag/v0.1.0
