# 0006. Control-plane stack and transport choices (proposed)

- **Status:** Superseded by ADR-0007 for the single-user build; retained as history
- **Date:** 2026-11-10
- **Deciders:** platform, security

## Context

ADR-0001 fixes the layers; it does not choose what the platform is built with.
The plan carried these choices in a table, which duplicated the decision record
format and drifted from it. They are collected here so there is one place to
change them before Phase 0 starts.

## Decision

| Decision | Recommendation | Why |
| --- | --- | --- |
| Control plane language | Rust (axum + sqlx) or Python/FastAPI reusing `hermes_cli` models | Rust matches the client's rigor; Python reuses host schema code. Pick one now; both must share the OpenAPI contract |
| Datastore | Postgres for control plane; SQLite stays host-local; S3-compatible for transcripts/artifacts | Tenancy and audit need relational constraints; host keeps its simple local model |
| Sandbox isolation | Container + gVisor first, microVM (Firecracker/Kata) for Enterprise | Shipping speed now, stronger isolation where it is sold |
| Screen transport | CDP screencast first, WebRTC later | Cheapest path to "watch any screen"; WebRTC when latency demands it |
| Events | NATS JetStream | Durable, simple, good k8s story |
| Auth | OIDC + device pairing; SCIM/SAML for Enterprise | Standard, testable, enterprise-ready |
| Billing | Usage events → ledger → monthly invoice | Auditable, reconcilable |

Nothing here is implemented. Each row becomes Accepted (with its own ADR if the
choice is significant) when the phase that needs it starts.

## Consequences

- The plan links here instead of restating the table, so a change is one edit.
- Phase 0 cannot start until the language and datastore rows are Accepted.
- Choosing Python for the control plane keeps schema sharing but adds a second
  runtime to operate; choosing Rust keeps one toolchain but needs the schema
  restated in SQL.
