# Architecture decision records

One file per decision. Copy [`template.md`](template.md). A decision is
“Accepted” when the code or plan follows it; supersede rather than rewrite.

| ADR | Decision | Status |
| --- | --- | --- |
| [0001](0001-three-layer-architecture.md) | Split client, host agent and platform; keep the host usable without a control plane | Accepted |
| [0002](0002-shared-cloud-computer.md) | One cloud computer per user, shared by that user's bots; per-bot screens | Accepted |
| [0003](0003-connector-token-brokering.md) | Broker connector tokens server-side; never place them in a sandbox | Accepted |
| [0004](0004-sync-and-revisions.md) | Append-only transcripts with revisions; last-writer-wins for settings | Accepted |
| [0005](0005-omarchy-desktop-integration.md) | Integrate through Omarchy's documented surfaces (theme files, notifier, shell plugins, launch helpers) | Accepted |
| [0006](0006-control-plane-stack.md) | Control-plane language, datastore, isolation, screen transport, events, auth and billing | Superseded by 0007 for the personal build |
| [0007](0007-single-user-node-hub.md) | One owner, outbound node agents, Tailscale UI path, SQLite, Helm/ArgoCD and brokered secret prompts | Accepted |
