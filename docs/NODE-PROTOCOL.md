# Node protocol v1

**Status:** implemented in `crates/hermes-protocol`, `crates/hermes-hub` and
`crates/hermes-node`; verified by `cargo test -p hermes-hub --test e2e`.
**Version:** 1. **Date:** 2026-11-10.

This is the contract behind the personal hub: one owner, one hub, one
`hermes-node` on every computer, and device sockets for the GPUI client. It is
deliberately not an RPC framework. The hub routes typed messages; a node is the
only component that touches a computer.

## Topology

```
GPUI device ──WSS /v1/device──► hermes-hub (k3s, OCI)
                                    │ SQLite: principals, nodes, commands,
                                    │         events, prompts, audit
                                    │
                              outbound WSS /v1/node
                                    │
                              hermes-node on each computer
                              (shell now; PTY/files/screen later)
```

Neither nodes nor devices accept inbound connections. The hub never dials a
computer. Tailscale is only the path from a device to the hub (and optionally a
Connector for direct SSH); it is not what connects nodes.

## HTTP surfaces

| Method | Path | Auth | Purpose |
| --- | --- | --- | --- |
| `GET` | `/healthz`, `/api/v1/health` | none | liveness and protocol version |
| `GET` | `/readyz` | none | database readiness |
| `POST` | `/api/v1/enrollments` | admin | mint a one-time enrollment code |
| `POST` | `/api/v1/enroll` | enrollment code | exchange for a principal id + token |
| `GET` | `/api/v1/nodes` | admin | node registry and presence |
| `DELETE` | `/api/v1/nodes/{id}` | admin | revoke node, close socket, fail its commands |
| `GET` | `/api/v1/devices` | admin | device principals and presence |
| `DELETE` | `/api/v1/devices/{id}` | admin | revoke device immediately |
| `POST` | `/api/v1/commands` | admin | submit a command by HTTP |
| `GET` | `/api/v1/commands/{id}` | admin | command record |
| `GET` | `/api/v1/commands/{id}/events?after=N&limit=M` | admin | replay stream events by sequence |
| `GET` | `/api/v1/audit?limit=N` | admin | personal audit, newest first |
| `GET` | `/api/v1/prompts` | admin | pending prompt metadata |
| `DELETE` | `/api/v1/prompts/{id}` | admin | cancel a pending prompt |
| `GET` | `/v1/node` | node token | node WebSocket upgrade |
| `GET` | `/v1/device` | device token | device WebSocket upgrade |

Admin means `Authorization: Bearer <HERMES_HUB_ADMIN_TOKEN>`. Node and device
tokens are separate principals and are rejected on the wrong surface. Tokens
and enrollment codes are returned exactly once; only SHA-256 hashes are stored.

## Pairing

```bash
# owner, once per computer
curl -fsS -X POST https://hermes/api/v1/enrollments \
  -H "Authorization: Bearer $HERMES_HUB_ADMIN_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"kind":"node","name":"forge"}'
# {"code":"…","kind":"node","name":"forge","expires_at_ms":…}

curl -fsS -X POST https://hermes/api/v1/enroll \
  -H 'content-type: application/json' -d '{"code":"…"}'
# {"id":"node-…","token":"…","kind":"node","name":"forge"}
```

The code expires after ten minutes by default and can be consumed once. The
long-lived token is shown once; losing it means revoking and enrolling again.
For a device, use `"kind":"device"`; the token belongs in the client's secret
store, never in argv or a transcript.

## Frame format

Every WebSocket text frame is one JSON object:

```json
{"protocol":1,"type":"heartbeat","at_ms":1762750000000,"running":[]}
```

- `protocol` is the wire version. A mismatch closes the connection with an
  `error` frame; there is no downgrade.
- `type` is the message tag. Unknown tags fail decoding.
- Frames over 1 MiB are rejected before parsing. Stream chunks are capped at
  64 KiB decoded.
- The first node/device frame must be `hello` within 10 seconds. It carries the
  principal id, which must equal the token's principal; a token cannot claim
  another computer.

Enums are `snake_case`. Commands carry argv as an array and are executed without
a shell. There is no string form of the command line on the wire.

## Node ↔ hub messages

### Node → hub

| Type | Fields | Notes |
| --- | --- | --- |
| `hello` | `node_id`, `agent_version`, `host{hostname,os,arch,agent_version}`, `capabilities`, `labels` | first frame only |
| `heartbeat` | `at_ms`, optional `load`, `running[]` | every `heartbeat_ms`; also refreshes presence |
| `command_started` | `command_id`, `at_ms`, optional `pid` | hub marks the command running |
| `stream` | `command_id`, `seq`, `channel`, `data_b64` | stdout/stderr/progress/file/screen/pty; standard base64 |
| `command_result` | `command_id`, terminal `status`, optional `exit_code`, `duration_ms`, optional `message` | exactly one terminal status wins |
| `prompt_request` | `prompt{id,node_id,command_id,challenge,requester,purpose,scope,env_var,created_at_ms,expires_at_ms}` | asks the hub to show a card |
| `prompt_closed` | `prompt_id` | node no longer needs an answer |

### Hub → node

| Type | Fields | Notes |
| --- | --- | --- |
| `hello_ack` | `node_id`, `heartbeat_ms`, `max_command_timeout_ms` | before any command |
| `command` | `command{id,idempotency_key,argv,cwd,env,timeout_ms,secret?}` | one delivery per idempotency key |
| `cancel` | `command_id` | node kills the child and reports `cancelled` |
| `prompt_answer` | `answer{prompt_id,challenge,decision}` | `provide{value,remember}` or `cancel` |
| `ping` | `at_ms` | node replies with a heartbeat |
| `error` | `code`, `message` | `replaced` closes a superseded node socket |

Capabilities are advertised, not inferred: `shell`, `pty`, `files`, `screen`,
`browser`, `gpu`, `services`, `secret_prompts`. Version 1 advertises shell and
secret prompts; the rest are `false` until their executors exist.

## Device ↔ hub messages

### Device → hub

| Type | Fields |
| --- | --- |
| `hello` | `device_id`, `name` |
| `submit_command` | `node_id`, `command` |
| `cancel_command` | `command_id` |
| `prompt_answer` | `answer` |
| `ping` | `at_ms` |

### Hub → device

| Type | Fields | Notes |
| --- | --- | --- |
| `hello_ack` | `device_id`, `heartbeat_ms` | |
| `node_list` | `nodes[]` | snapshot on connect |
| `node_presence` | `node` | online/offline transitions |
| `command_state` | `command` | queued, running, terminal |
| `command_stream` | `node_id`, `command_id`, `seq`, `channel`, `data_b64` | live output |
| `prompt_request` | `prompt` | render a masked card |
| `prompt_closed` | `prompt_id` | answered, cancelled, expired or node gone |
| `pong` | `at_ms`, `server_at_ms` | |
| `error` | `code`, `message` | request rejected, never a secret value |

A device that reconnects receives a fresh node list and every still-pending
prompt. Stream history is retrieved with the REST events route using the last
sequence it applied, so a reconnect does not require a replay buffer on the
device socket.

## Command lifecycle

1. Owner or device submits a request. The hub validates argv, timeout, env names
   and secret scope.
2. The command row is inserted with `(node_id, idempotency_key)` unique. A retry
   returns the first record and is **not** delivered again.
3. The hub sends `command` only if the node socket is connected. An offline node
   returns `409` and the command is failed; it is never silently queued.
4. The node applies its own policy: allowed roots, optional program allowlist,
   cleared environment, execution timeout, cancellation. A refused command is
   reported as `denied`, not executed.
5. Output is streamed with a monotonically increasing `seq` per command and
   persisted in `command_events` for replay.
6. One terminal `command_result` wins. A late duplicate cannot rewrite a
   succeeded or cancelled command.

The hub never persists `env` or the secret spec; `CommandRecord` contains only
argv, cwd, status, timings and exit code. The request is held in memory for the
single delivery.

## Secret prompt sequence

```
node                       hub                         device
 |                          |                            |
 | prompt_request(challenge)|                            |
 |------------------------->| prompt_request             |
 |                          |--------------------------->| masked card
 |                          |                            |
 |                          |<---------------------------| provide(value)
 |                          | validate challenge+expiry  |
 | prompt_answer(value)     |                            |
 |<-------------------------| prompt_closed              |
 |                          |--------------------------->|
 | inject env, run child    | audit: prompt.answered     |
 | command_result           | (no value)                 |
 |------------------------->|                            |
```

- The challenge is 32 random bytes, base64, single use. The answer must echo it.
- A wrong challenge burns the prompt; there is no retry against a live
  challenge.
- Expiry, cancellation, device revocation, node disconnect and a closed card all
  resolve the prompt and send `prompt_closed`. With no device online the hub
  cancels immediately rather than leaving the node hanging.
- The value exists only in the answer frame, in memory in the hub and node, and
  in the child environment. It is zeroized after injection. It is never in the
  transcript, audit detail, command record, argv, logs or shell history.
- `remember` is parsed but v1 does not persist it; OS keyring support is Phase 2.
- Sudo password delivery over stdin is not implemented yet. The env-var path is
  for credentials such as `PGPASSWORD`; do not treat it as a PTY.

## Reconnect and failure semantics

| Event | Behaviour |
| --- | --- |
| Device socket drops | reconnect, receive node list + pending prompts, replay events with `after` |
| Node socket drops | hub marks it offline and fails its queued/running commands; node cancels local children |
| Node reconnects | registers again, gets a fresh `hello_ack`; old socket is closed with `replaced` |
| Duplicate node socket | newest wins; older is told `replaced` and closed, and its unfinished commands fail rather than hanging |
| Duplicate command submission | first record returned, no second execution |
| Prompt expiry | hub resolves it, broadcasts `prompt_closed`, sends `cancel` to the node |
| Hub restarted | nodes reconnect and SQLite restores registry/history; unresolved prompts close as `hub_restarted` because their one-time challenges are gone |
| Revoked token | WebSocket handshake fails; existing socket is closed by the hub |

Version 1 does **not** buffer a running command across a node reconnect: the
hub fails in-flight commands and the node kills them. Resumable execution is a
Phase 1 feature. Device reconnect replay is implemented.

## Security properties and limits

- Bearer tokens are 256-bit random values stored as SHA-256 hashes. Admin
  comparison is constant-time. Enrollment codes are single-use and expire.
- Node and device tokens are scoped by principal kind and id; a node cannot send
  results for another node's command.
- The node clears the child environment: `HERMES_NODE_TOKEN`, the hub URL and
  any host secret are not inherited. Only `PATH`, `HOME`, `LANG`, `TERM`, the
  request's explicit env and the prompted secret are passed.
- The node enforces allowed roots after canonicalization, so `..` and symlinks
  cannot escape. Program allowlists match the basename or full argv[0].
- No rate limiting, mTLS or at-rest encryption yet. Terminate TLS at the
  Tailscale ingress (or a reverse proxy) and protect the SQLite volume; the
  application-level bearer token is still required.
- One hub replica only: SQLite is a single writer. HA and Postgres are not part
  of v1.
- Shell execution is the only executor. PTY, files, screen, browser and GPU are
  represented in the protocol and capability manifest but not implemented.

## Adding a message

1. Add the variant to `hermes-protocol`, preserving the version and existing
   tags. A breaking shape is version 2, not a reinterpretation of version 1.
2. Validate it at the boundary (size, ownership, expiry) in the hub handler.
3. Add a protocol round-trip test and an end-to-end test that exercises the
   failure path as well as the happy path.
4. Update this document and the capability row in `ROADMAP.md`.
