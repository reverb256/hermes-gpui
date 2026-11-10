# Full parity plan: Grok Bot + Hermes

**Status:** planning document, 2026-11-10. `PARITY.md` remains the per-feature
status list; this file is the build plan behind it. Nothing here is implemented
merely because it is written down.

## 1. The honest shape of the problem

There are three layers, and only the first one is "porting Hermes":

| Layer | What it is | Today |
| --- | --- | --- |
| **Client** | `hermes-gpui` — GPUI desktop app | ~55% of the Hermes Desktop surface; see `PARITY.md` |
| **Host agent** | `hermes-agent` — FastAPI + JSON-RPC gateway running on one machine | Mature single-user agent: sessions, tools, MCP, cron, memory, skills, voice, files, browser, computer use, Kanban |
| **Platform** | Everything Grok Bot has that a single-host agent cannot have | **Does not exist** |

Grok Bot is not a better desktop client for a local agent. It is a hosted
product: one persistent cloud computer per user, a fleet that keeps working when
the laptop closes, shared bots with permissions and analytics, an app catalog
with brokered credentials, org-level policy and audit, metering and billing, and
sync across desktop and phone. None of that can be produced by adding screens to
the client.

So "full parity" means building a platform around `hermes-agent`, while
`hermes-agent` itself gains the multi-user seams it currently lacks.

## 2. What already exists (verified in this workspace)

**Host agent (`Projects/hermes-agent`)**

- REST: `/api/sessions`, `/api/profiles`, `/api/mcp/*`, `/api/cron/*`,
  `/api/memory/*`, `/api/skills/*`, `/api/webhooks/*`, `/api/files*`,
  `/api/fs/*`, `/api/messaging/platforms*`, `/api/providers/*`,
  `/api/audio/speak`, `/api/audio/transcribe`, `/api/pairing*`,
  `/api/portal`, `/api/analytics/*`, `/api/ops/*`, `/api/actions/*`.
- JSON-RPC gateway: session lifecycle, `prompt.submit`, approvals, tool
  started/completed events, `browser.*`, PTY bridge.
- Tools: `tools/computer_use/` (display automation), `tools/browser_*`,
  `tools/pty_session.py`, `tools/voice` paths via `hermes_cli/voice.py`.
- Plugins: `kanban` (boards, tasks, comments, attachments, runs, workers,
  dispatch), `memory`, `platforms`, `browser`, `image_gen`, `video_gen`,
  `observability`, `dashboard_auth`, `cron_providers`.
- Auth: one dashboard session token (`HERMES_DASHBOARD_SESSION_TOKEN`),
  optional basic auth, pairing. **Per-host, not per-user.**

**Client (`Projects/hermes-gpui`)**

- Chat, streaming, reasoning, Markdown, approvals (partial), sessions with
  resume/export, profiles, MCP + OAuth, skills hub, cron, memory, MoA, Kanban
  (boards, tasks, comments, attachments, field edits), single-host config,
  tray/IPC, Omarchy theming.

**Absent everywhere**

- Accounts, organisations, teams, roles, invitations, SSO/SCIM.
- Per-user cloud computers, snapshots, screen streaming, takeover.
- Shared bots, group chat, bot-to-bot messaging, templates, bot analytics.
- App connector catalog with brokered OAuth tokens the bot never sees.
- Cross-device sync, push notifications, guaranteed background execution.
- Event-triggered routines (email/Slack/webhook) with ownership and retries.
- Artifact store with provenance and sharing.
- Audit log of tool calls, policy decisions and admin events.
- Metering, entitlements, budget enforcement, invoices.
- Retention, DLP, network policy, per-team policy engine.

## 3. Workstreams that are not a port

Each row is a build programme with its own service, schema and exit criteria.
"Depends on" is the true critical path.

### A. Identity, tenancy and access

Why not a port: the host has one token and one user. Grok Bot has accounts,
members, roles, seats, and strict per-user isolation.

Build:

- `hermes-control` service (new): Postgres schema for `accounts`, `orgs`,
  `members`, `roles`, `bots`, `shares`, `devices`, `sessions_meta`,
  `usage_events`, `audit_events`, `policies`, `templates`.
- OIDC login (email magic link, Google, GitHub), device pairing flow, refresh
  tokens, short-lived per-sandbox tokens.
- RBAC: owner / admin / manager / member / viewer, with a permission matrix
  enforced server-side on every control-plane route.
- Invitations, seat assignment, entitlement lookup, org switching.
- SCIM + SAML for Enterprise; service accounts for team-managed connectors.
- Isolation tests: cross-tenant reads must fail at the storage layer, not just
  in a UI check.

Depends on: nothing. **Start here.** Everything else needs an identity.

Exit criteria: two accounts cannot see each other's bots, sessions, files,
usage or audit events under any route; permission matrix covered by tests.

### B. Cloud computer fleet

Why not a port: Hermes `computer_use` drives a display on the host it runs on.
Grok Bot gives every user a persistent sandbox with browser, filesystem and
terminal, keeps it running while the client is closed, and streams its screen.

Build:

- Sandbox provisioner on k3s: one pod per user (`Xvfb` + Chromium + CDP +
  file service + PTY + agent host), per-user PVC, resource quotas, idle
  suspend/resume, snapshot and restore (restic/CSI), image build pipeline.
- Screen streaming: CDP `Page.startScreencast` (or `ffmpeg` + WebRTC for
  full desktop) → control-plane relay → client. View-only first, then
  input injection with takeover arbitration.
- Per-bot virtual screens on the shared computer (one computer-use task per bot
  screen at a time, matching the documented Grok model).
- Lifecycle: create, start, drain, restart, destroy; health and capacity
  accounting; stuck-sandbox reaper.
- Egress and network policy hooks (default-deny egress allowlist).

Depends on: A (ownership, tokens, quotas).

Exit criteria: close the client mid-turn, reopen on another device, work is
still running and the transcript continues; snapshot restores an identical
browser session; one user cannot address another's sandbox.

### C. Collaboration substrate

Why not a port: the host is single-player. Shared bots, group chat and bot
handoffs are control-plane features.

Build:

- Sharing: share a bot with a member or team, scopes (chat, files, routines,
  analytics), revocation, "shared by" attribution.
- Group chat: multi-member rooms, multiple bots per room, turn arbitration,
  per-bot responsibility, handoff messages.
- Bot-to-bot messaging: addressing, delivery, transcript links, loop and rate
  guards, permission gate (a bot may only message bots it shares a room with).
- Templates: versioned bot config export without secrets, import, gallery,
  provenance.
- Analytics: weekly/daily active users, messages, hours saved, top users — from
  usage events, not client telemetry.

Depends on: A; B for shared-computer semantics; D for connector scoping.

Exit criteria: a bot shared with a teammate answers in their chat on their
computer, cannot read the owner's private files, and every shared action is
attributed in audit.

### D. Connectors, apps and credentials

Why not a port: MCP exists, but there is no catalog, no token broker, no
team-managed credentials, no per-connector policy.

Build:

- Connector catalog service: apps, scopes, health, per-team enablement.
- OAuth broker: tokens held server-side, tools invoked by reference; the bot
  and the sandbox never receive the token.
- Per-user vs team-managed credentials, rotation, revocation, break-glass.
- Vault integration for user secrets; strict redaction in logs, transcripts and
  error surfaces.
- Email identity per user (own address, inbound routing into routines).
- MCP parity: per-tool selection, schemas, cost hints, profile scope.

Depends on: A.

Exit criteria: an app can be connected, used, revoked, and blocked by team
policy without the sandbox ever holding the token; secrets never appear in
transcripts, logs or crash reports.

### E. Sync, continuity and notifications

Why not a port: session history lives in a per-host SQLite file; there is no
cross-device state, no push channel.

Build:

- Control-plane sync of session metadata, transcripts (object storage),
  drafts, bot configs, routines, read state; optimistic concurrency with a
  clear conflict rule (append-only transcripts, last-writer-wins for settings).
- Device registry and push (APNs/FCM) + desktop notifications with action
  routing, per-bot preferences and quiet hours.
- Background guarantees: routine and turn completion events survive client
  disconnect; resumable streams with sequence numbers.
- Offline client behavior: queued intents are rejected visibly today — keep
  that, and add an explicit outbox only where a server-side idempotency key
  exists.

Depends on: A, B.

Exit criteria: the same conversation, draft and bot state appear on desktop and
phone within seconds; a notification opens the exact turn; duplicate delivery
is idempotent.

### F. Scheduling and triggers

Why not a port: cron exists per host; event triggers with ownership, approvals
and retries do not.

Build:

- Trigger service: cron, webhook, inbound email, Slack/Teams events, file-drop.
- Ownership and permission: who may create, who approves, collaborator toggles.
- Delivery guarantees: at-least-once with idempotency keys, dead-letter queue,
  backoff, run history, cancel.
- Budget guards: a routine cannot exceed its account's remaining budget.

Depends on: A, E for notification, I for budgets.

Exit criteria: a misconfigured trigger retries without duplicate side effects;
a revoked collaborator's routines stop immediately.

### G. Artifacts and files

Why not a port: `/api/files` is a local file browser; there is no artifact
store, provenance or sharing.

Build:

- Artifact store (S3-compatible) with content hashes, producing turn/bot, MIME,
  size, retention.
- Preview pipeline (image/audio/video/PDF/code), signed URLs, quotas.
- Sharing rules tied to bot/file permissions; download audit.
- Provenance: which tool, which run, which inputs produced it.

Depends on: A, B (files originate in sandboxes).

Exit criteria: an artifact can be found, previewed, downloaded and shared
without exposing another user's objects; every download is attributable.

### H. Governance, policy and audit

Why not a port: Hermes logs for operators; Grok Bot has enforceable policy and
an audit trail of decisions.

Build:

- Audit pipeline: every tool call and outcome, approval decision (auto-review,
  hook, member), connector call, shell command, navigation, computer-use
  session, file transfer, outbound message, routine run, guardrail hit, skill
  read, delegation. Append-only, queryable, exportable.
- Policy engine: team rules, connector allow/block, network allowlist,
  local-execution ceiling, auto-review rules, approval expiry, DLP patterns.
- Admin console surfaces (API first, UI later) and security event stream.
- Retention and legal hold.

Depends on: A; feeds from B, D, F.

Exit criteria: for any action a reviewer can answer "who, what, why allowed,
what happened" from audit alone; policy changes are themselves audited.

### I. Metering and billing

Why not a port: `/api/analytics/usage` reports local provider cost. Grok Bot
meters usage per account, enforces budgets and bills overage.

Build:

- Usage events from the gateway (tokens in/out, tool calls, computer-seconds,
  storage) with account/bot attribution.
- Aggregation jobs, budget caps with hard stop, overage reporting.
- Entitlements per plan/seat, invoice export, admin cost views.
- Cost attribution for shared bots (whose usage pays).

Depends on: A, H (same event spine).

Exit criteria: a budget cap actually blocks new work at the boundary; invoices
reconcile with usage events to the cent in a test harness.

### J. Reliability and operations platform

Why not a port: one host is easy to operate; a multi-tenant fleet is not.

Build:

- Per-tenant tracing across client → control plane → sandbox → provider.
- SLOs (turn start latency, stream continuity, sandbox availability), error
  budgets, alerting.
- Backups and restore drills for control-plane Postgres and sandbox volumes.
- Migration tooling, canary deploys, rollback, load tests (1000 concurrent
  streams), chaos tests (kill a sandbox mid-turn).

Depends on: A, B.

### K. Distribution

Why not a port: a desktop binary is not a product.

Build:

- Signed packages (deb/rpm/AppImage/Flatpak, macOS, Windows), auto-update
  channel, crash reporting with consent, telemetry opt-out.
- Companion mobile app (view, approve, notify) — smallest useful surface first.
- Web client for approval and history.
- Deep links (`hermes://session/...`, `hermes://bot/...`) and OS share targets.

## 4. Host-agent gaps to close inside `hermes-agent`

These are extensions, not a rewrite:

1. **AuthN/AuthZ middleware** — accept control-plane JWTs, scope every route by
   account/bot, remove reliance on a single dashboard token for API access.
2. **Tenant-scoped storage** — per-account paths, no shared SQLite between
   accounts, explicit quota enforcement.
3. **Event bus** — publish tool/approval/routine/usage events to the control
   plane (NATS or Redis Streams) instead of only local logs.
4. **Screen streaming endpoints** — screencast subscribe, input injection with
   takeover tokens, per-bot screens.
5. **Artifact APIs** — register, list, fetch signed URLs, provenance.
6. **Sync APIs** — session metadata and transcript revisions with sequence
   numbers, idempotent appends.
7. **Approval hardening** — expiry, replay protection, multiple concurrent
   requests, reconnection replay, choice validation.
8. **Typed tool results** — structured payloads (not just text) so cards can
   render diffs, files, images and errors.
9. **Streaming voice** — STT/TTS duplex with interruption and device selection.
10. **Pagination everywhere** — sessions, messages, runs, audit export.

## 5. Client work remaining (hermes-gpui)

Sequenced against the phases below; this is `PARITY.md` filtered to what the
client can own:

- Projects/workspaces: first-class project model, worktree picker, multi-session
  tiles.
- Transcript: search, virtualization for large histories, selection, edit/retry,
  fork, link activation with scheme allowlist.
- Files and artifacts: browser, previews, downloads, uploads to the sandbox.
- Terminal: PTY transport, resize, multiple sessions, scrollback.
- Computer: screen view, takeover request, input, per-bot screens.
- Voice: capture, device pickers, call state, transcripts.
- Notifications and approvals: action routing, per-bot preferences, quiet hours.
- Accessibility: focus order, labels, screen-reader pass, scaling, reduced
  motion, IME verification on all three platforms.
- Packaging, auto-update, crash reporting, multi-host switching.
- Command palette and complete keyboard map; i18n scaffolding.

## 6. Architecture decisions to lock before Phase 1

Decisions live in the records, not here: locked choices in
[ADR-0001–0005](adr/README.md), and the still-open stack, transport, auth and
billing choices in [ADR-0006](adr/0006-control-plane-stack.md).


## 7. Phased roadmap

Effort is engineering-months for one focused engineer; parallelisable across
workstreams once Phase 0 lands.

| Phase | Scope | Exit demo | Est. |
| --- | --- | --- | --- |
| **0. Tenancy spine** | `hermes-control` skeleton, Postgres schema, OIDC login, sandbox provisioner, per-user token, client host/login UI | Two users log in, each gets an isolated sandbox, neither can see the other | 2–3 |
| **1. Cloud computer MVP** | Per-user sandbox, screen view-only stream, file browser, terminal, snapshot/resume | Close client mid-turn; reopen elsewhere; watch the same turn finish | 3–4 |
| **2. Takeover, approvals, audit** | Input injection with takeover tokens, policy engine, audit pipeline, team rules | Every action traceable; a blocked action is blocked server-side | 3 |
| **3. Collaboration** | Sharing, group chat, bot-to-bot handoff, templates | Shared bot answers a teammate on their own computer, attributed | 3–4 |
| **4. Connectors and vault** | Catalog, OAuth broker, team credentials, email identity, MCP parity | Connect an app, revoke it, prove tokens never reach the sandbox | 3 |
| **5. Sync and notifications** | Cross-device sync, push, resumable streams | Same state on desktop and phone; notification opens the turn | 2–3 |
| **6. Triggers and routines** | Event triggers, retries, approvals, budgets | Webhook/email trigger runs exactly once with a budget cap | 2 |
| **7. Artifacts** | Store, previews, sharing, provenance | Artifact found, previewed, shared, audited | 1–2 |
| **8. Metering and billing** | Usage events, ledger, entitlements, invoices | Cap blocks work; invoice reconciles with events | 2 |
| **9. Enterprise** | SSO/SCIM, network policy, DLP, retention, private deploy | Admin enables a team, enforces egress, exports audit | 3–4 |
| **10. Hardening/GA** | SLOs, load/chaos, pen test, DR drill, packaging/updates | 1000 concurrent streams; sandbox kill recovers; restore drill passes | 3–4 |

Critical path: **A → B → (C, D, F, G) → E → H → I → enterprise → GA**, with the
client's terminal/screen/artifact/voice surfaces tracking Phases 1, 3, 5, 7.

## 8. Verification strategy (per capability, not per release)

- **Contracts**: every new route lands with a client-side contract test against
  a fake server (already the pattern in `crates/hermes-api`) and a live test
  against a disposable host (`HERMES_GPUI_LIVE_HOST`), as done for Kanban.
- **Isolation matrix**: automated tests asserting 403/404 across tenants for
  every route class: sessions, files, artifacts, bots, routines, audit, usage.
- **Policy tests**: table-driven allow/deny cases for roles × actions.
- **Sandbox tests**: kill mid-turn, restart, snapshot/restore, quota, egress
  denial, takeover arbitration.
- **Billing tests**: ledger replay from events must reproduce invoices.
- **Client**: accessibility pass with a screen reader, keyboard-only walkthrough,
  IME checks on Wayland/X11, contrast/scale checks, visual review on an isolated
  display.
- **Security**: threat model per phase, secret-leak tests (transcripts, logs,
  crash reports), pen test before GA.

## 9. Risk register

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Shared computer leaks context between bots | High | Document it as a boundary (Grok does), per-bot screens, explicit warnings, optional per-bot sandbox tier |
| Secrets exposed to the sandbox | High | Broker tokens server-side; never inject into env; redaction tests |
| Sandbox cost and idle waste | Medium | Idle suspend, quotas, capacity forecasting, per-account budgets |
| Screen streaming latency | Medium | CDP screencast first, WebRTC where needed, region-local relays |
| Multi-tenant data leakage | Critical | Storage-layer scoping, isolation matrix in CI, pen test |
| Model/provider terms and rate limits | Medium | Provider abstraction already exists; per-account keys; fallback routing |
| Scope creep into a full SaaS build | High | Phase gates with exit demos; no phase starts before its dependency exits |

## 10. What "done" means

Full parity is done when all of the following are true and demonstrated:

1. A new account signs in, gets a private cloud computer, and works with a bot
   from desktop and phone with nothing lost across devices.
2. Work continues with the client closed; the user can watch and take over any
   bot's screen.
3. A bot shared with a teammate works in their chat without exposing the owner's
   private data, with usage attributed and auditable.
4. Every tool call, approval and admin change is answerable from audit.
5. A team admin can enforce connector, network and local-execution policy.
6. Usage is metered, capped and invoiced from the same events that drove the
   work.
7. The desktop client covers the functions in `PARITY.md` marked client-side,
   with accessibility and packaging verified on the platforms claimed.

Until those hold, `PARITY.md` stays the source of truth and no parity claim is
made.
