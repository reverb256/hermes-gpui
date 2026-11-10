# Feature parity inventory: Hermes Desktop and Grok Bot

**Status: incomplete.** This is a detailed working inventory, not proof of full
parity. It covers the Hermes Desktop source checkout at `c13c93588c` and Grok
Bot's public documentation reviewed on 2026-11-10. Product releases, hidden or
account-specific features, and deployment-specific plugins can add requirements.
Do not count an item complete merely because a backend route exists.

Legend: **Implemented** = client code exists; **Partial** = only the stated
subset exists; **Missing** = no equivalent client feature identified;
**Unknown** = behavior or host support needs investigation. Unless stated
otherwise, implemented features are not yet verified end-to-end against a live
host. Some requirements need backend, infrastructure, platform, security or
product work beyond this GPUI repository.

## Chat and interaction

| Capability | Status / next work |
| --- | --- |
| Stream chat and reasoning | Implemented; validate reconnect and long-running turns. |
| Show tool activity | Partial: text events; structured tool results, rich details and progress need parity audit. |
| Submit, interrupt and start a conversation | Implemented; verify active-turn resume and interruption against host. |
| Send another instruction while a turn is active | Missing; verify host steer/queue semantics and surface them safely. |
| Approval and clarification cards | Partial; audit every gateway request type, choices, expiration, multiple concurrent requests and reconnect replay. |
| Secret, password and sudo prompts | Unknown; audit gateway prompt types and safe masking. |
| Markdown, code blocks, syntax highlighting, copy | Partial: CommonMark block parsing, headings, lists, quotes and code blocks; inline styles, links, syntax highlighting and copy remain. |
| Links and safe external navigation | Partial; audit schemes, confirmation and remote content handling. |
| Image, file and other attachments | Partial: file picker/upload; audit multi-file, paste, previews, size limits and failures. |
| Inline images | Partial: explicit load for standalone host-local PNG/JPEG/WebP references; no general media or arbitrary remote fetching. |
| Reply to a specific message and reactions | Missing; requires a compatible persistence and gateway contract. |
| Editable drafts and review before external send | Missing; needs host-supported draft/approval lifecycle. |
| Message copy, selection, editing, retry and fork | Missing; verify host semantics before implementing. |
| Search within transcripts, navigation and large histories | Missing; measure virtualization and loading on real large sessions. |
| Composer text selection, cursor navigation, undo/redo, IME | Partial: GPUI editable-text primitive, labeled text input and native selection/IME handling; end-to-end keyboard, assistive technology and platform validation remain. |
| Keyboard shortcuts and command palette | Partial; audit actual behavior and reachability against both products. |
| Mentions | Partial: profile mention UI; groups, routines, apps and completion semantics missing. |
| Accessibility | Missing audit: keyboard focus, screen readers, labels, contrast, scaling and reduced motion. |

## Sessions, projects and workspaces

| Capability | Status / next work |
| --- | --- |
| Session list, search, pin, archive, rename and delete | Implemented; verify paging, profile scope, destructive confirmation and errors. |
| Resume and persisted transcript | Partial: gateway resume with read-only REST fallback; verify lineage, active turns, large history and fallback behavior. |
| JSON export | Implemented with user-chosen save path; verify host profile and large exports, file permissions and interrupted writes. |
| Project filtering | Partial: filters first loaded session page by `cwd`; not a full project model or server-wide search. |
| Projects, create/edit/delete and project settings | Missing; inspect first-class project routes and permissions. |
| Workspace chooser, working directory and Git worktrees | Missing; includes branch/worktree selection and validation. |
| Multiple simultaneous session tiles and layout | Missing; Hermes Desktop multi-session/workspace behavior needs audit. |
| Session history pagination, filtering and sort | Partial; currently loads at most 50 sessions. |
| Continue across devices | Backend-dependent; session storage exists, but sync/conflict and cross-client behavior unverified. |
| Import/export formats, share links and retention | Partial: JSON export only; import, sharing, retention controls absent. |

## Profiles, bots and collaboration

| Capability | Status / next work |
| --- | --- |
| Profile roster, switcher and detail inspector | Partial; validate scope across every REST and gateway action. |
| Avatar, display metadata, toolsets and profile secrets | Partial; existing inspector supports a subset; audit full profile configuration. |
| Create, duplicate, rename and delete profiles/bots | Missing; include confirmations and lifecycle effects. |
| Bot pinning, sections, hiding and search | Partial: roster search; pinning, sections and hiding missing. |
| Per-bot instructions, model, memory, skills and routines | Partial; audit profile-scoped editing and isolation end to end. |
| Bot-to-bot messaging and delegation | Unknown; requires host capability, routing, transcript and permission model. |
| Group chat, members, mentions and hand-offs | Missing; backend protocol and shared ownership required. |
| Shared bots, collaboration roles and access management | Missing; requires backend identity, authorization, discovery and audit. |
| Bot templates and share links | Missing; define what configuration is exported and protect secrets. |
| Bot analytics and shared-bot usage | Missing; backend metrics and privacy controls required. |
| Background work when client closes | Backend-dependent; validate daemon lifecycle, progress and notifications. |

## Skills, tools, MCP and connectors

| Capability | Status / next work |
| --- | --- |
| List/search/enable installed skills | Implemented; verify profile scope. |
| Skill hub discovery, preview, scan and install | Implemented; verify host contract, trust and failure paths. |
| Skill authoring, editing, removal and version management | Missing; inspect host capability and secure file access. |
| Teach a task by demonstration | Missing; requires secure computer capture, draft/review and host workflow. |
| Private/team skill sharing and permissions | Missing; requires identity and backend support. |
| MCP configured servers, enable, test, add, edit, delete | Partial; audit profile scope and concurrent configuration edits. |
| MCP catalog discovery and installation | Implemented; verify background install outcomes and credentials. |
| MCP OAuth browser hand-off | Implemented against start/status/cancel routes; live OAuth, cancellation, URL launch failures and non-default profile behavior need verification. |
| MCP per-tool selection, schemas and costs | Missing; inspect host contract and authorization. |
| Application connector catalog, OAuth, disconnect and health | Partial: MCP and provider account surfaces only; Grok-style app connectors not generally implemented. |
| Connector account selection, credential ownership and sharing | Missing; backend identity/secret isolation required. |
| Toolsets and detailed computer-use configuration | Partial: inspector toggles; full settings and permissions missing. |
| General plugin install, enable, disable and contributed views | Missing; Hermes Desktop plugin SDK and lifecycle need separate compatibility design. |

## Scheduling and Kanban

| Capability | Status / next work |
| --- | --- |
| List cron jobs, create, pause/resume, run, delete, history | Implemented; verify actual host responses, timezone and all error paths. |
| Cron editing, richer schedules, timezone and delivery | Partial; validate supported fields and full edit lifecycle. |
| Event-triggered routines and webhook triggers | Missing; requires host event integration and access controls. |
| Routine ownership, shared access and approvals | Missing; backend permissions and audit needed. |
| Kanban current board and basic status transitions | Partial: `/api/plugins/kanban/board` and task PATCH; optional plugin required; transitions are host-validated. |
| Kanban create/edit/delete tasks, comments and attachments | Partial: title-only creation in triage, title editing and confirmed deletion; comments, attachments and other fields remain. |
| Board selection, creation, settings, archive and deletion | Partial: local selection with per-request board scoping; creation, settings, archive and deletion remain. |
| Drag/drop, filters, search, bulk actions and live updates | Missing; plugin supports events; define conflict/error behavior. |
| Assignees, dependencies, scheduling, review and workers | Missing beyond simple next-status action; respect transition constraints. |
| Task detail, diagnostics, runs, logs and recovery | Missing; plugin routes exist; inspect permission and data sensitivity. |
| Dispatch, model overrides, decomposition and estimates | Missing; these can start work or incur model costs, so explicit action and disclosure are required. |
| Kanban project/worktree settings, orchestration and notifications | Missing; plugin exposes routes, but backend availability varies. |

## Artifacts, computer and files

| Capability | Status / next work |
| --- | --- |
| Artifact browser, search and provenance | Missing; inspect host artifact APIs and storage semantics. |
| Image/audio/video/document/code previews and downloads | Partial: host-local image preview and JSON export only. |
| Canvas/interactive artifact rendering and share links | Missing; isolate untrusted content and define host support. |
| Remote file browser, upload, download and workspace operations | Missing; audit host filesystem APIs, authorization, traversal and allowed roots. |
| Embedded terminal, output, resize and multiple terminals | Missing; needs safe PTY transport and session management. |
| Embedded browser and live computer screen | Missing; needs host-supported display/session transport and isolation. |
| Watch/take over computer use and demonstrate workflows | Missing; needs remote control arbitration, permissions and audit. |
| Persistent shared computer, browser sessions and files | Backend/infrastructure work; Hermes GPUI is a client and does not provision a Grok-like cloud computer. |
| User isolation, bot screens, execution approvals and reset | Backend/infrastructure/security work; no parity claim without threat modeling. |
| Connected-device local execution | Missing; explicit per-device approval and audit required. |

## Voice and sound

| Capability | Status / next work |
| --- | --- |
| Speak last assistant reply | Partial: external `voice-models` TTS plus local audio player; verify connectivity and errors. |
| Desktop dictation | Partial: external Superwhisper can paste text; no in-app capture or device picker. |
| Voice notes, waveform, playback and transcript | Missing; define formats, retention and safe media access. |
| Live voice conversation, streaming STT/TTS, interruption | Missing; needs backend voice protocol, audio capture and explicit permissions. |
| Select voice, language, microphone and playback device | Missing in UI; only environment-based TTS configuration exists. |
| Mute, hang up, call state and call transcript | Missing. |

## Models, accounts, memory and security

| Capability | Status / next work |
| --- | --- |
| Provider/model picker, reasoning and auxiliary assignments | Partial; verify all host response shapes, current model and profile scope. |
| MoA presets, activation, reference models and aggregator | Partial: editor of existing presets; verify complete schema and concurrent updates. |
| Provider OAuth sign-in, polling and disconnect | Implemented for supported host flows; needs live verification and server-side cancellation audit. |
| Provider endpoints, fallback models, credential pools | Missing; inspect Hermes Desktop settings and backend contract. |
| Secrets vault and profile-specific secret edits | Partial; audit redaction, confirmation, permissions and no secret logging. |
| Memory provider selection, config and confirmed reset | Partial; memory browsing/editing, review, per-profile scope and retention need audit. |
| Human approval settings, auto-review, scoped execution policies | Missing; requires host-side enforcement, not UI-only controls. |
| Identity, multi-user RBAC, team sharing and audit logs | Missing; backend/product architecture required. |
| Auth sessions, logout, tokens, cookie refresh and TLS | Partial; login exists; audit credential lifecycle, rotation and failure states. |
| Account usage, billing and cost reporting | Partial: existing analytics and billing links; feature and provider coverage unverified. |

## Desktop, Omarchy and operations

| Capability | Status / next work |
| --- | --- |
| Native GPUI application, no Electron | Implemented; Linux execution checked in isolated Xvfb. |
| Omarchy theme following | Implemented; validate light/dark/custom palettes, contrast and live updates. |
| Singleton, `--toggle`, tray show/quit | Implemented; validate Wayland/X11 minimize, visibility, stale socket and race handling. |
| Dedicated Omarchy bar widget and status | Missing; tray action and example keybind are not a bar widget. |
| Launch at login, package/install/update/uninstall | Missing or unknown; audit packaging and lifecycle for target platforms. |
| Desktop notifications, sound, focus and privacy | Missing; include action routing, per-bot preferences and disabled states. |
| Multi-host selection, SSH/remotes and credentials | Missing; host currently configured at startup. |
| Appearance, font scaling, shortcuts and user preferences | Partial: theme sync only; settings persistence and key rebinding missing. |
| Resizing and small-screen layout | Unknown; inspect responsive behavior and settings sheet overflow. |
| Cross-platform builds and accessibility APIs | Unknown; no macOS/Windows parity claim from Linux testing. |
| Offline errors and reconnect behavior | Partial: disconnected actions fail visibly rather than being replayed; REST is not independently usable while gateway is offline. |
| CI, tests, versioning and licensing | Partial: fmt/clippy/tests exist; add contract, integration, security and release checks as features mature. |

## Sources and validation gaps

- Hermes Desktop routes: `apps/desktop/src/app/routes.ts`; settings in
  `apps/desktop/src/app/settings/`; Kanban plugin API in
  `plugins/kanban/dashboard/plugin_api.py` and desktop `plugins/kanban/api.ts`.
- MCP OAuth: `hermes_cli/web_routers/mcp.py` and
  `apps/desktop/src/lib/mcp-dashboard-oauth.ts`.
- Grok Bot public docs: [chat and collaboration](https://docs.x.ai/grok-bot/chat-and-collaboration),
  [bots](https://docs.x.ai/grok-bot/bots), [computer and apps](https://docs.x.ai/grok-bot/computer-and-apps),
  [skills and routines](https://docs.x.ai/grok-bot/skills-routines-and-automations),
  [shared bots](https://docs.x.ai/grok-bot/shared-bots), and
  [settings and notifications](https://docs.x.ai/grok-bot/settings-and-notifications).
  Public search excerpts were available; direct page extraction timed out.
- Validate against a safe disposable Hermes host and test accounts; no live
  OAuth, credential deletion, cron creation, task dispatch or real data changes
  were performed for this inventory. Visual QA must stay isolated from the user's
  active desktop.
