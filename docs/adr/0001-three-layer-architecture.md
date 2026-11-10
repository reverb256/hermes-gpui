# 0001. Three-layer architecture: client, host agent, platform

- **Status:** Accepted for layers 1–2; layer 3 updated by ADR-0007 for the single-user build
- **Date:** 2026-11-10
- **Deciders:** client and platform maintainers

## Context

`hermes-agent` is a mature single-user agent host: FastAPI routes, a JSON-RPC
gateway, tools, MCP, cron, memory, skills, voice and plugins, authenticated by a
single dashboard session token. Grok Bot is a hosted product: per-user cloud
computers, shared bots, org policy, audit and metering.

Trying to make one process both a local agent host and a multi-tenant SaaS would
destroy the simplicity that makes the host easy to run, and would put tenancy
concerns into a codebase with no user model.

## Decision

Keep three layers with explicit contracts:

1. **Client** — `hermes-gpui`, talks REST + JSON-RPC to a host and, later, to the
   control plane.
2. **Host agent** — `hermes-agent`, runs per user (or per sandbox) and stays
   usable standalone with no control plane.
3. **Platform** — `hermes-hub` for the personal build (one owner, outbound node
   agents, audit and sync), with a sandbox fleet as later work. The original
   multi-tenant `hermes-control` design is withdrawn; see ADR-0007.

The client feature-detects: a plain host still works; control-plane features
appear when the host/control plane advertises them.

## Consequences

- The host must gain auth middleware, tenant-scoped storage and event
  publication, but never owns billing or org policy.
- Host-only deployments remain supported and are the test target for the client.
- Two deployments to operate (host + control plane) and an extra network hop for
  platform features.
- Wire contracts become first-class: control plane and host both need OpenAPI,
  and the client needs contract tests per surface.
