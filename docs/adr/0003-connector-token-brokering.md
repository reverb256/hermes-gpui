# 0003. Broker connector tokens server-side; never place them in a sandbox

- **Status:** Accepted
- **Date:** 2026-11-10
- **Deciders:** platform, security

## Context

Connected apps require OAuth tokens. A sandbox with browser and filesystem
access can read anything placed in its environment or home directory, and any
bot on the shared computer can read another bot's environment. Storing tokens in
the sandbox would make one prompt-injection or malicious skill enough to exfiltrate
every connected account.

## Decision

Hold connector tokens in the control plane's credential service and expose
**tool invocation by reference**: the agent asks the connector service to perform
an action; the service attaches the token and returns the result. The sandbox
never receives the token, and MCP connectors that cannot be brokered are marked
as such and require explicit user consent. Team-managed connectors use
service-account credentials the member cannot read.

## Consequences

- Connector implementation moves from "MCP config with env vars" to a brokered
  service with per-call authorization and audit.
- Latency and availability of the broker become part of every connector call;
  it needs tight SLOs and caching of non-secret metadata only.
- Revocation is immediate and central, which also makes audit and policy
  enforcement feasible.
- Some community MCP servers require local credentials; they must run in an
  explicitly-labelled lower-trust mode or not at all.
