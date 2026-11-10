# Roadmap

**Last updated:** 2026-11-10. **Status:** planning. **Scope:** one person, their
own hosts and devices.

This is the single roadmap for reaching Grok Bot + Hermes parity *for a single
user*. Feature-level status lives in [`PARITY.md`](PARITY.md); architecture,
risks and verification strategy live in
[`docs/FULL-PARITY-PLAN.md`](docs/FULL-PARITY-PLAN.md).

Status values: **Not started**, **In progress**, **Blocked**, **Done**.
A phase is only **Done** when its exit demo has been run and recorded.

## What this is not

Explicitly out of scope unless the user reverses the decision:

- Teams, organisations, members, invitations, roles and RBAC.
- Shared bots, group chat, bot-to-bot collaboration, templates as share links.
- Team-managed connector credentials, per-team policy, SSO/SCIM, admin console.
- Metering, plans, entitlements, invoicing and billing.
- Multi-tenant isolation and fleet capacity management.

Those are withdrawn from the plan, not deferred. The personal equivalents that
remain are a single owner, paired devices, a personal audit log and self-imposed
cost caps.

## Two scopes

- **Client parity** (`hermes-gpui` only, sprints S1–S4): every gap the client can
  close against the current host. Exit: those rows are **Done** or **Blocked** on
  a named host contract.
- **Single-user parity** (client + host + personal hub): phases 0–7 below.
  Exit: the conditions in
  [`docs/FULL-PARITY-PLAN.md`](docs/FULL-PARITY-PLAN.md#10-what-done-means).

Single-user parity is 15–20 engineering-months including integration: roughly
8–11 months with two engineers, 15–18 with one. Client parity (S1–S4) is 6–8
weeks and lands first because it is useful without the hub.

## Where we are

| Layer | State |
| --- | --- |
| Client (`hermes-gpui`) | In progress — chat, sessions, profiles, skills, MCP, cron, memory, Kanban details/comments/attachments, transcript find, Omarchy theme/notifications/bar widget, building PKGBUILD |
| Host agent (`hermes-agent`) | Mature single-user agent; needs token auth for remote access, pagination, typed events, screen/input, sync, artifacts and voice |
| Personal hub (`hermes-hub`) | In progress — pairing, host registry, command routing, audit and secret-prompt broker built; Tailscale/ArgoCD deployment and client picker pending |

## Target shape

```
GPUI client ──Tailscale/WSS──► hermes-hub (k3s, OCI)
                                  │  volumes: SQLite, sync, artifacts
                                  │  node registry + command router
                                  └──► hermes-node on every connected computer
                                        forge, nexus, sentry, zephyr, laptops
                                        └─ local executor: shell/PTY, files,
                                           screen, browser, GPU, host services
```

The hub is single-owner by construction: one admin credential, no orgs, roles
or tenancy columns. It does not reach into machines; every computer runs
`hermes-node`, which dials **out** to the hub over WSS/mTLS, registers its
capabilities and executes commands locally under its own policy. This is the
Grok Bot shape: the installed client is the agent on that computer, and the
remote model acts on any connected computer through the hub.

The GPUI client can embed the same node runtime, so the machine running the UI
is also one of the computers Hermes can act on. Tailscale is only the path from
the UI to the hub, plus an optional direct LAN path for SSH; it is not what
connects the computers. There is no fleet or provisioner.

## Deployment

Everything server-side ships as OCI images and runs on any OCI runtime:

- `hermes-hub` — pairing, device and host registry, sync, push, audit index.
- `hermes-computer` — agent host plus an isolated desktop: Xvfb, Chromium/CDP,
  PTY, files. Headless variants can skip Chromium and Xvfb.

State lives in named volumes: hub SQLite, host data, browser profile, artifact
directory (or an S3-compatible endpoint). The hub is deployed by a Helm chart in
Git and synced by ArgoCD; the chart also carries the optional Tailscale
Connector. `hermes-node` is installed on each computer as a systemd user
service, an OCI container, or bundled with the GPUI client; all three speak the
same node protocol. Supported hub targets, in preference order:

1. Helm chart via ArgoCD on the existing k3s cluster (primary).
2. `docker compose` or `podman-compose` on a single host.
3. Direct `docker run`/`podman run` for a minimal install.

Remote access uses the **Tailscale Kubernetes operator already running in the
cluster**. The hub is published with a Tailscale Ingress
(`hermes.<tailnet>.ts.net`), so paired devices join the tailnet and reach it with
no inbound port. Tailscale ACLs restrict those devices to the hub and, if
enabled, the four host IPs. A hub-issued short-lived device token remains a
second factor; Tailscale identity alone is not enough to call the API.

Node reachability does not depend on a VPN: each `hermes-node` keeps an
outbound connection to the hub, so NAT, changing addresses and laptops work
without routes. For direct host access (SSH, per-host agent ports, debugging),
add one or two Tailscale **Connector** resources advertising the host LAN CIDR
(`<host-lan-cidr>`), shared by forge, nexus, sentry and zephyr. The CRD exists in
the cluster but no Connector is deployed yet. Prefer two Connectors on different
nodes for HA. Do not advertise the pod/service CIDRs unless a concrete client
need appears.

Pangolin is not part of the default path: it duplicates the existing Tailscale
overlay with its own Traefik/Badger/Gerbil stack and a browser-oriented auth flow
that fits an API/WebSocket desktop client poorly. It remains an optional
public-web front for browser dashboards, not the transport. Plain WireGuard is
kept as a fallback transport profile for installations without Tailscale;
neither VPN is required for node connectivity.

Snapshots are volume archives (restic or k8s volume snapshots where available),
not a custom image format, so restore works on any runtime.

## Node protocol

`hermes-node` is the installed agent on each computer. The protocol is versioned
and runs over an authenticated WSS/mTLS connection:

- `register` — hostname, OS/arch, node version, labels, trust roots and a
  capability manifest (shell, PTY, files, screen, browser, GPU, services, secret
  prompts).
- `heartbeat` — presence, load and current jobs.
- `command` — id, idempotency key, timeout, cancellation token, approval token,
  working root and argv. Secrets never travel in argv.
- `stream` — stdout/stderr, PTY bytes, screen frames, file chunks and progress.
- `prompt` — typed request for a secret, password, sudo password or credential,
  with a one-time challenge, requester context, allowed scope and expiry.
- `event` — tool, approval, audit and usage events for the hub and personal
  audit.

Reconnect resumes by sequence number; a command is idempotent by key; an offline
node rejects rather than queues unless the command is explicitly resumable.
Local policy can always refuse a command and the hub cannot bypass it. Protocol
version negotiation happens at registration, so older nodes fail closed.

### Secret and credential prompts

Secret prompts are cards, never terminal echo. The node opens a one-time
challenge and the client shows a card naming the requester, purpose, scope and
expiry, with masked input, show/hide, paste, submit and cancel. The value travels
only in the prompt response over the authenticated channel, is never written to
the transcript, logs, crash reports, command line or shell history, and is
zeroized after use. “Remember” is opt-in and uses the OS keyring (Secret
Service); sudo passwords are fed on stdin, never argv. Closing the card, a
reconnect or expiry invalidates the challenge.

## Phase roadmap

| Phase | Scope | Exit demo | Est. (eng-mo) | Status |
| --- | --- | --- | --- | --- |
| **0. Personal spine** | Helm chart + ArgoCD app for `hermes-hub`; node protocol v1 (register, heartbeat, capabilities, command, cancel, resume, prompt); `hermes-node` on all four hosts; pairing, node registry and capability map; Tailscale Ingress and optional Connector; client login/node picker; personal audit skeleton | A paired device reaches the hub over Tailscale with no inbound port; all four node agents are connected and one remote command has run on each; revoking the device cuts access | 2–3 | **In progress** — protocol, hub, node shell executor, prompt broker, audit, chart, GPUI node picker/command surface and credential cards built; the hub runs in k3s through ArgoCD and one node (`zephyr`) is enrolled, so three hosts remain |
| **1. Node execution** | Local executors on `hermes-node`: shell/PTY, files, screen (Xvfb or real display), browser/CDP, GPU; optional `hermes-computer` container for an isolated desktop; named volumes; view-only screen stream, terminal, file browser, snapshot/restore, client surfaces | Close the client mid-turn, reopen on another device, watch the same turn finish; restoring the volume brings back the same browser state | 2–3 | **In progress (shell and terminal)** — argv execution, streaming, timeout, cancel and a PTY terminal with input/resize are implemented; files, screen, browser, GPU and resume are not |
| **2. Approvals, secret prompts, takeover, audit** | Secret/password/sudo/credential prompt cards with one-time challenges; input injection with a takeover lease; approval expiry/replay/idempotency; typed tool results; personal audit from tool/approval/shell events | Every action answers what happened and why from the personal audit; a secret card completes without the value appearing in transcript, logs or argv; a blocked action is blocked server-side | 2–2.5 | **Not started** — the prompt broker, challenge/expiry/replay rejection and audit slice were built early in Phase 0; client cards, sudo stdin, approvals and takeover remain |
| **3. Connectors and vault** | Connector catalog, server-side OAuth broker, personal credential vault, rotation/revocation, MCP per-tool schemas and costs | Connect an app, use it, revoke it; the computer never holds the token | 2 | Not started |
| **4. Sync and notifications** | Revisioned transcripts, drafts and read state, idempotent appends, resumable streams, device registry, desktop/mobile push with action routing | Same state on two devices within seconds; a notification opens the exact turn | 2 | Not started |
| **5. Triggers and routines** | Cron, webhook and email triggers; retries, dead-letter, run history and self-imposed cost caps | A misconfigured trigger retries without duplicate side effects and stops at its cap | 1.5 | Not started |
| **6. Artifacts** | Artifact store with hashes and provenance, previews, signed URLs, retention, client browser | Artifact found, previewed, downloaded and attributable without exposing the host filesystem | 1.5 | Not started |
| **7. Hardening and GA** | Backup/restore drills, 24 h soak, signed client packages, auto-update, crash reporting, accessibility, optional phone companion | Restore drill passes; signed install updates itself; accessibility walkthrough passes | 2 | Not started |

**Critical path:** 0 → 1 → (2, 3, 5, 6) → 4 → 7.

## Workstreams

| ID | Workstream | Phase | Status |
| --- | --- | --- | --- |
| A | Personal identity, Tailscale ingress, Helm/ArgoCD, pairing and hub | 0 | In progress — hub, chart and enrolment exist; not deployed |
| B | Node agents, capability registry, remote execution and screen/input relay | 0–2 | In progress — outbound agent, shell execution and streams; screen/input pending |
| C | Connectors, vault and credentials | 3 | Not started |
| D | Sync, continuity and notifications | 4 | Not started |
| E | Triggers and routines | 5 | Not started |
| F | Artifacts and files | 6 | Not started |
| G | Reliability, backup and restore | 7 (continuous) | Not started |
| H | Distribution and updates | 4, 7 | Not started |

## Client track (hermes-gpui)

Sprint order is the commitment; a row moves only when its exit check passes.
Anything marked **Blocked** stays out of the UI rather than shipping a stub.

| Sprint | Item | Depends on | Status |
| --- | --- | --- | --- |
| S1 | `--version` / `--help` CLI | none | Not started |
| S1 | Command palette coverage and complete key map | none | Not started |
| S1 | Markdown link activation with scheme allowlist | none | Not started |
| S1 | Correct stale parity/roadmap claims | none | In progress (this update) |
| S2 | Transcript find | none | Done — `Ctrl+F`, highlighting, match cycling |
| S2 | Older-turn pagination, virtualization, text selection | host pagination; none for virtualization | In progress — search done; pagination, virtualization and selection remain |
| S2 | Edit/retry/fork, replies, reactions, drafts | host persistence and gateway lifecycle | Blocked |
| S3 | Projects/workspaces and worktree picker | host routes exist; needs contract audit | Not started |
| S3 | Multi-session tiles and layout | none | Not started |
| S4 | Kanban plugin contract audit (board fields, events, runs) | disposable host + plugin source | Not started — next client action |
| S4 | Board metadata, drag/drop, live updates | verified plugin schema and events | Blocked until S4 audit |
| S4 | Task dispatch, runs, diagnostics, recovery | verified routes plus cost/approval disclosure | Blocked |
| S5 | Node/host picker with live capabilities and presence | Phase 0 | Implemented for shell commands and terminals — device socket, roster/presence, argv-only execution, output/cancel, PTY sessions and credential cards; file/screen/GPU executors remain |
| S5 | File browser, embedded terminal, screen view/takeover | Phase 1–2 | Partial — one live terminal per client over the node PTY with input and resize; file browser and screen view/takeover are not started |
| S5 | Secret/password/sudo/credential prompt cards | Phase 2 node prompt API | Partial — protocol, challenge validation, audit and the masked client cards are wired; sudo-over-stdin and approvals remain |
| S6 | Artifact browser and previews | Phase 6 | Not started |
| S6 | Voice capture, device pickers, call state | host voice work | Not started |
| S6 | Notification action routing and per-bot preferences | Phase 4 | Not started |
| S7 | Accessibility, IME and scaling validation | none | Not started |
| S7 | Packaging auto-update, crash reporting, uninstall | Phase 7 | Partial — PKGBUILD builds and installs the binary, desktop entry and widget |
| Later | i18n scaffolding | none | Optional |

### Omarchy desktop track

Details and commands in [`docs/OMARCHY.md`](docs/OMARCHY.md).

| Item | Status |
| --- | --- |
| Theme following: Omarchy `colors.toml` only, semantic tokens, 2 s refresh, `theme-set` hook | Implemented; no other theme sources or overrides |
| Single instance, `--toggle`, tray show/quit | Implemented |
| `omarchy-notification-send` on turn completion and approvals, click-to-focus, `--replace-id` collapsing, reminder action | Implemented (approval cards notify with Open + Remind; no in-notification Approve) |
| `omarchy-shell` bar widget reading `status --json` (state, approvals, tooltip) | Implemented in `packaging/omarchy-shell-hermes`; joining `omarchy.agents` not attempted |
| `omarchy-launch-or-focus` binding, `.desktop` entry and PKGBUILD | Implemented: entry + PKGBUILD ship in `packaging/`; package build verified, live bar binding not yet run |
| Screenshot attachment via `omarchy capture`; OCR prompt via `omarchy capture text` | Implemented as composer actions “▣ Shot” and “T Text” |
| Superwhisper-aware composer affordance | Not started (dictation itself is Superwhisper's) |
| `--diagnostics`, `status --json`, `--install-omarchy-hooks` | Implemented |

## Host-agent extensions (`hermes-agent`)

Ordered by what it unblocks. Dependencies in parentheses.

0. **Contract snapshot** — generate and commit the host's OpenAPI/JSON-RPC route
   and event inventory; every later change diffs against it (none, week 1).
1. Token auth for remote access, with scoped device tokens (Phase 0).
2. Cursor pagination for sessions, messages, runs and audit (Phase 0,
   continuous).
3. Typed tool results and event publication for tools, approvals, routines and
   usage (Phase 2).
4. Approval hardening: expiry, replay protection, concurrency, reconnect replay
   (Phase 2).
5. Secret prompt API: one-time challenge, requester scope, expiry and a
   response channel that never logs (Phase 2).
6. Screen streaming and input injection endpoints with takeover leases
   (Phase 1–2).
7. Sync APIs: revisions, idempotent appends, resumable sequences (Phase 4).
8. Artifact APIs with provenance and signed fetch (Phase 6).
9. Streaming voice: duplex, interruption, device selection (when voice work
   starts).

## Execution gates

1. **Contract before UI**: every new control maps to a committed route, field or
   event, covered by a fake-server test and a live disposable-host test.
2. **Access control**: only paired, scoped devices reach the hub or host; a
   revoked device or node fails immediately; Tailscale guards the UI path and the
   hub enforces its own device token; no inbound port.
3. **Secrets**: redaction tests across transcripts, logs, crash reports,
   command lines and shell history. A prompt value is one-time, scoped, masked,
   expires with its challenge and is never fed through argv; the personal computer
   never receives brokered connector tokens.
4. **Resilience**: kill mid-turn, restart, snapshot/restore, quota and
   egress-denial tests.
5. **Cost**: usage is visible and self-imposed caps block at the boundary.
6. **GitOps only**: every cluster change lands as a Helm chart value or template
   in git and syncs through ArgoCD; no kubectl-only mutations. `helm lint`,
   `helm template` and a kubeconform pass run in CI.
7. **Reproducible images**: hub and computer images build from pinned
   Dockerfiles/Containerfiles, are tagged by version and digest, and run under
   k3s, Docker and Podman before a deployment is called done.
8. **Evidence in the same commit**: a row changes status only with its test,
   live run or drill linked from the commit.
9. **Client accessibility**: keyboard-only and screen-reader walkthrough on
   Wayland and X11; visual QA on an isolated display, never the active desktop.

## Open decisions

These change the plan and need an ADR before the affected phase starts:

| Decision | Affects | Default if unresolved |
| --- | --- | --- |
| First runtime for the OCI images: k3s, Docker or Podman | 0–1 | k3s on the existing cluster, with compose as the portable fallback |
| Which machine runs `hermes-computer`: `nexus`, `zephyr` or `forge` | 1 | `nexus`, where the agent data already lives |
| Node transport: outbound WSS/mTLS through the hub, or direct Tailscale path | 0 | **Decided in ADR-0007:** outbound WSS; Tailscale only for the UI and optional direct SSH |
| Node runtime: systemd user service, OCI container, or bundled in the GPUI client | 0 | **Decided:** systemd and OCI ship now; bundling is Phase 1 |
| Secret “remember”: OS keyring or hub vault | 2 | OS keyring (Secret Service); the hub vault is for connector tokens only; `remember` is parsed but not persisted in v1 |
| Sync devices: desktop only, or desktop + phone companion | 4, 7 | Desktop first, phone later |
| Auth: device pairing tokens or an OIDC login | 0 | **Decided:** pairing tokens; no OIDC in the personal build |
| Platforms at GA: Linux/Omarchy only, or macOS/Windows too | 7 | Linux/Omarchy only |

## Milestones

| Milestone | Definition | Phase |
| --- | --- | --- |
| M0 “Private remote” | A paired device reaches the hub over Tailscale without an inbound port; all four node agents are connected and one action has run on each | 0 |
| M1 “Keeps working” | A turn survives client exit; screen, terminal and files resume | 1 |
| M2 “Accountable” | Every tool call and approval is answerable from the personal audit; a secret prompt completes without leaking its value | 2 |
| M3 “Connected” | Connector tokens are brokered, revocable and never in the computer | 3 |
| M4 “Everywhere” | Same state on two devices; a notification opens the exact turn | 4 |
| M5 “Hands off” | A trigger runs with retry safety and a cost cap | 5 |
| M6 “Deliverables” | An artifact is found, previewed, downloaded and traced | 6 |
| M7 “GA” | Restore drill passes, signed install updates itself, accessibility passes | 7 |

## How this roadmap is maintained

- A phase moves to **In progress** only when its dependency phase has exited.
- A row moves to **Done** only with a linked verification record (test, live
  run, or drill) in the same commit as the status change.
- Scope changes go in [`docs/FULL-PARITY-PLAN.md`](docs/FULL-PARITY-PLAN.md) and
  are reflected here in the same commit.
- Per-feature client status lives in [`PARITY.md`](PARITY.md); this roadmap
  tracks phases, the client sprint order and only the client work that blocks a
  phase.
- Team, organisation, sharing, SSO/SCIM, metering and billing work is out of
  scope. Reintroducing any of it requires a roadmap change first.
- No parity claim is made in `README.md` until `PARITY.md` and this roadmap
  agree.
