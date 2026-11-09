# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Skills-hub browser: source-wide search, SKILL.md and manifest preview, the
  installer's security verdict, and confirmed installs by identifier.
- Voice playback: `▷ Speak` synthesizes the last assistant reply through the
  stack's `voice-models` service and plays it through PipeWire/Pulse.
- Session controls: pin/unpin and archive rows through the host's
  `PATCH /api/sessions/{id}`, with the list refreshed from the host.
- Memory page: active provider, discovery status and built-in file sizes from
  `GET /api/memory`, plus activation for providers the host marks ready.

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
