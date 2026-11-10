# Roadmap

**Last updated:** 2026-11-10. **Status:** planning. This is the single roadmap
for reaching Grok Bot + Hermes parity. Feature-level status lives in
[`PARITY.md`](PARITY.md); the reasoning, architecture decisions, risks and
verification strategy live in [`docs/FULL-PARITY-PLAN.md`](docs/FULL-PARITY-PLAN.md).

Status values: **Not started**, **In progress**, **Blocked**, **Done**.
A phase is only **Done** when its exit demo has been run and recorded.

## Where we are

| Layer | State |
| --- | --- |
| Client (`hermes-gpui`) | In progress — chat, sessions, profiles, skills, MCP, cron, memory, Kanban with details/comments/attachments, sessions paging, reply copy, board archive/delete |
| Host agent (`hermes-agent`) | Mature single-user agent; needs multi-tenant seams |
| Platform (control plane, cloud computers, collaboration, billing) | Not started |

## Phase roadmap

| Phase | Scope | Exit demo | Est. (eng-mo) | Status |
| --- | --- | --- | --- | --- |
| **0. Tenancy spine** | `hermes-control` skeleton, Postgres schema (accounts, orgs, members, roles, bots, devices), OIDC login, device pairing, sandbox provisioner, per-user tokens | Two users sign in, each gets an isolated sandbox, neither can see the other | 2–3 | Not started |
| **1. Cloud computer MVP** | Per-user sandbox (Xvfb + Chromium + CDP + files + PTY), screen view-only stream, file browser, terminal, snapshot/resume | Close the client mid-turn, reopen elsewhere, watch the same turn finish | 3–4 | Not started |
| **2. Takeover, approvals, audit** | Input injection with takeover tokens, policy engine, audit pipeline, team rules | Every action traceable; a blocked action is blocked server-side | 3 | Not started |
| **3. Collaboration** | Shared bots with scopes, group chat, bot-to-bot handoff, templates, shared-bot analytics | Shared bot answers a teammate on their own computer, attributed in audit | 3–4 | Not started |
| **4. Connectors and vault** | Connector catalog, OAuth broker, team-managed credentials, per-user email identity, MCP parity | Connect an app, revoke it, prove tokens never reach the sandbox | 3 | Not started |
| **5. Sync and notifications** | Cross-device sync, push (APNs/FCM + desktop), resumable streams, device registry | Same state on desktop and phone; notification opens the exact turn | 2–3 | Not started |
| **6. Triggers and routines** | Webhook/email/Slack triggers, ownership, approvals, retries, dead-letter, budgets | Webhook trigger runs exactly once with a budget cap enforced | 2 | Not started |
| **7. Artifacts** | Artifact store, provenance, previews, sharing, retention | Artifact found, previewed, shared and audited | 1–2 | Not started |
| **8. Metering and billing** | Usage events, ledger, entitlements, budget caps, invoices | Cap blocks work at the boundary; invoice reconciles with events | 2 | Not started |
| **9. Enterprise** | SSO/SCIM, network policy, DLP, retention, private deployment, admin console | Admin enables a team, enforces egress, exports audit | 3–4 | Not started |
| **10. Hardening and GA** | SLOs, load/chaos tests, pen test, DR drill, signed packaging, auto-update | 1000 concurrent streams; sandbox kill recovers; restore drill passes | 3–4 | Not started |

Critical path: **0 → 1 → (2, 3, 4, 6, 7) → 5 → 8 → 9 → 10**.

## Workstreams (parallel tracks)

Each workstream has a dedicated section in
[`docs/FULL-PARITY-PLAN.md`](docs/FULL-PARITY-PLAN.md#3-workstreams-that-are-not-a-port).

| ID | Workstream | Phase | Status |
| --- | --- | --- | --- |
| A | Identity, tenancy, access | 0 | Not started |
| B | Cloud computer fleet | 1–2 | Not started |
| C | Collaboration substrate | 3 | Not started |
| D | Connectors, apps, credentials | 4 | Not started |
| E | Sync, continuity, notifications | 5 | Not started |
| F | Scheduling and triggers | 6 | Not started |
| G | Artifacts and files | 7 | Not started |
| H | Governance, policy, audit | 2, 9 | Not started |
| I | Metering and billing | 8 | Not started |
| J | Reliability and operations | 10 (continuous) | Not started |
| K | Distribution | 5, 10 | Not started |

## Client track (hermes-gpui)

Maps to [`PARITY.md`](PARITY.md); only the items the client can own.

| Item | Depends on phase | Status |
| --- | --- | --- |
| Projects/workspaces, worktree picker | none (host routes exist) | Not started |
| Transcript search, virtualization, selection, retry/fork | none | Not started |
| Multi-session tiles and layout | none | Not started |
| File browser and uploads | 1 | Not started |
| Embedded terminal | 1 | Not started |
| Computer screen view, takeover, per-bot screens | 1–2 | Not started |
| Artifact browser and previews | 7 | Not started |
| Voice capture, device pickers, call state | host voice work | Not started |
| Notifications and approval routing | 5 | Not started |
| Accessibility, IME and scaling validation | none | Not started |
| Packaging, auto-update, crash reporting | 10 | Not started |
| Multi-host selection and credentials | 0 | Not started |
| Command palette and complete key map | none | Not started |
| i18n scaffolding | none | Not started |
| Drag-and-drop and live Kanban updates | none | Not started |
| Task runs, diagnostics, recovery, dispatch | Kanban plugin routes | Not started |

### Omarchy desktop track

Details and commands in [`docs/OMARCHY.md`](docs/OMARCHY.md).

| Item | Status |
| --- | --- |
| Theme following (`colors.toml`, semantic tokens, `theme.generation`, `shell.toml` metrics, `theme-set` hook) | Implemented; hook installs with `--install-omarchy-hooks` |
| Single instance, `--toggle`, tray show/quit | Implemented |
| `omarchy-notification-send` on turn completion and approvals, click-to-focus, `--replace-id` collapsing, reminder action | Implemented (approval cards notify with Open + Remind; no in-notification Approve) |
| `omarchy-shell` bar widget reading `status --json` (state, approvals, tooltip) | Implemented in `packaging/omarchy-shell-hermes`; joining `omarchy.agents` not attempted |
| `omarchy-launch-or-focus` binding, `.desktop` entry and PKGBUILD | Implemented: entry + PKGBUILD ship in `packaging/`; binding documented, not yet run on a live bar |
| Screenshot attachment via `omarchy capture`; OCR prompt via `omarchy capture text` | Implemented as composer actions “▣ Shot” and “T Text” |
| Superwhisper-aware composer affordance | Not started (dictation itself is Superwhisper's) |
| `--diagnostics`, `status --json`, `--install-omarchy-hooks` | Implemented |

## Host-agent extensions (`hermes-agent`)

Sequenced so the client and platform can consume them:

1. AuthN/AuthZ middleware (control-plane JWTs, per-route scoping) — Phase 0.
2. Tenant-scoped storage and quotas — Phase 0.
3. Event bus publication (tools, approvals, routines, usage) — Phase 2.
4. Screen streaming and input injection endpoints — Phase 1–2.
5. Artifact APIs — Phase 7.
6. Sync APIs (revisions, idempotent appends) — Phase 5.
7. Approval hardening (expiry, replay, concurrency) — Phase 2.
8. Typed tool results — Phase 2.
9. Streaming voice (duplex, interruption, devices) — post-Phase 5.
10. Pagination everywhere — continuous.

## Milestones

| Milestone | Definition | Phase |
| --- | --- | --- |
| M0 “Two users” | Isolated accounts, sandboxes, no cross-tenant reads | 0 |
| M1 “Keeps working” | Turn survives client exit; screen view resumes | 1 |
| M2 “Accountable” | Audit answers who/what/why for any action | 2 |
| M3 “Teammates” | Shared bot works for a teammate, attributed | 3 |
| M4 “Connected” | Brokered app OAuth, revocable, never in sandbox | 4 |
| M5 “Everywhere” | Same state on two devices; push opens the turn | 5 |
| M6 “Hands off” | Event trigger runs with budget and retry safety | 6 |
| M7 “Deliverables” | Artifact lifecycle with provenance | 7 |
| M8 “Metered” | Budget cap and reconciling invoice | 8 |
| M9 “Governed” | Admin policy, SSO, network policy, audit export | 9 |
| M10 “GA” | SLOs met, pen test closed, restore drill passed | 10 |

## How this roadmap is maintained

- A phase moves to **In progress** only when its dependency phase has exited.
- A row moves to **Done** only with a linked verification record (test, live
  run, or drill) in the same commit as the status change.
- Scope changes go in [`docs/FULL-PARITY-PLAN.md`](docs/FULL-PARITY-PLAN.md) and
  are reflected here in the same commit.
- No parity claim is made in `README.md` until `PARITY.md` and this roadmap
  agree.
