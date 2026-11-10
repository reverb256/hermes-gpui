# 0007. Single-user hub, outbound node agents and SQLite

- **Status:** Accepted
- **Date:** 2026-11-10
- **Deciders:** platform, security
- **Supersedes:** the applicable rows of ADR-0006 for the personal build

## Context

The product is for one owner, not a tenant population. The client must act like
an installed agent on each computer, connect those computers together, and pop
up credential cards. The cluster already runs Tailscale and ArgoCD, and every
server-side component must be reproducible as an OCI image.

ADR-0001 keeps the host usable without a control plane. ADR-0006 listed a
multi-tenant control plane (Postgres, OIDC, SCIM, billing, NATS), which no longer
matches the scope. The build needs one decision that makes the first vertical
slice executable without pretending the later phases exist.

## Decision

1. **One hub, one owner.** `hermes-hub` is a Rust/axum service with SQLite
   (rusqlite, bundled). There are no org, role or tenant columns; the admin
   bearer token is the owner credential.
2. **Outbound node agents.** Every computer runs `hermes-node`; it dials the hub
   at `/v1/node` over WSS, authenticates with its own token and registers
   capabilities. The hub never dials back. Node reachability does not depend on a
   VPN.
3. **Tailscale for the UI path only.** The hub is published by a Tailscale
   Ingress. A Tailscale Connector is optional and only for direct SSH/LAN
   access. Pangolin is not the transport; raw WireGuard is a fallback profile.
4. **Typed, versioned protocol v1.** JSON frames with a `protocol` field,
   one-time enrollment, idempotent commands, streamed output and a prompt broker.
   Unknown versions fail closed.
5. **Helm in Git, synced by ArgoCD.** The chart owns the Deployment (one
   replica), SQLite PVC, Service, Tailscale Ingress and optional Connector. Compose
   is the portable fallback. Images are OCI and pinned by digest in production.
6. **Secret prompts are a protocol feature.** The node raises a challenge; the
   hub brokers it to a device; the value is forwarded once, never stored, never
   in argv and never audited. With no device connected the prompt fails closed.

## Consequences

- Phase 0 can run end to end today: enroll, connect, command, stream, prompt,
  audit and revoke are covered by `crates/hermes-hub/tests/e2e.rs`.
- SQLite limits the hub to one writer. That is acceptable for one owner; a
  future HA or multi-user build needs a new ADR and a different datastore.
- Tokens are bearer credentials, not mTLS identities. Tailscale ACLs restrict
  the network path; the hub still checks its own token, so tailnet membership
  alone is insufficient.
- The hub cannot resume a running command after a node disconnect in v1. It
  fails the command visibly instead of inventing an outcome; resumable
  execution is Phase 1.
- The client UI and the remaining executors are not implied by this decision.
  The protocol advertises only capabilities that exist.
- ADR-0006 remains historical context for the withdrawn multi-tenant plan.
