# Operations

**Last updated:** 2026-11-10. Covers running `hermes-desktop` against a host,
every environment variable, packaging status, verification gates and
troubleshooting. Omarchy-specific integration is in
[`OMARCHY.md`](OMARCHY.md).

## What you need

- A Hermes host reachable over HTTP(S) with:
  - the dashboard REST API (`/api/*`), and
  - the JSON-RPC gateway at `ws(s)://<host>/api/ws`.
- Credentials: bearer token, session cookie, or basic-auth user/password
  (gated hosts exchange basic auth for a cookie plus WS tickets).
- Optional: the Kanban plugin for the Kanban surface, a `voice-models` TTS
  service for **Speak**, and Superwhisper for dictation.

The client defaults to `http://127.0.0.1:9119` (loopback dashboard), which is
only right for a throwaway local host. In this fleet the backend is the
`hermes-crew` pod — see **Deployed backend: hermes-crew** below.

## Deployed backend: hermes-crew

In this fleet the host is not a laptop process: it is the `hermes-crew` pod in
namespace `hermes-crew` (one gateway, every profile, state on the `hermes-data`
PVC, memory in the in-cluster OpenRecall store). This client is its frontend.

| | |
| --- | --- |
| Endpoint on a k3s node | `http://10.43.29.77:9119` (Service VIP, pinned) |
| Endpoint off-cluster | `https://hermes-crew.tigris-ule.ts.net` (tailnet ingress) |
| Auth | `HERMES_TOKEN` = deployment session token, else basic login → cookie |
| Token source | BSM `hermes-crew-session-token` → Secret `hermes-crew/hermes-dashboard` key `session-token` → `HERMES_DASHBOARD_SESSION_TOKEN` |

**Status 2026-11-12:** the value exists in BSM, the Deployment reads it as an
optional key, but External Secrets has not delivered it — it fails with a 400
from the Bitwarden API while listing organization secrets (`GET
/rest/api/1/secrets` through `bitwarden-sdk-server`). Until that clears, the
gateway mints a random token per start and rejects the stored one, so the helper
probes `/api/profiles` with the bearer and falls back to password login when it
is refused.

```bash
source scripts/hermes-crew-env.sh   # resolves URL + credentials, exports env
```

The token is a deployment session token, not an OIDC/cookie credential: it is a
bearer on REST and a `?token=` on the WebSocket, so it is the only auth that
survives a gateway restart. `HERMES_BASIC` still works because the client
performs `/auth/password-login` (provider `basic`) itself, but the resulting
cookie dies with the gateway process.

The VIP is only routable from cluster nodes. The ingress is the path for any
other machine, and it is gated by the tailnet ACL — when the caller is not
admitted the ingress proxy logs `no rules matched` and the connection is
dropped, which looks like a dead backend from the client side. Check the ACL
before blaming the gateway.

## Environment variables

| Variable | Default | Effect |
| --- | --- | --- |
| `HERMES_URL` | `http://127.0.0.1:9119` | host origin; scheme/host/port only |
| `HERMES_TOKEN` | unset | bearer token for every REST call and WS query; on hermes-crew use the deployment session token |
| `HERMES_BASIC` | unset | `user:pass`; gated hosts trade it for a cookie + WS ticket |
| `HERMES_COOKIE` | unset | ready-made `__Host-hermes_session_at=…`; skips login |
| `HERMES_PROFILE` | host active profile | profile the chat is scoped to |
| `HERMES_TTS_URL` | unset | base URL of a `voice-models` service (`POST /tts`) |
| `HERMES_TTS_VOICE` | `en-US-AndrewNeural` | TTS voice name |
| `HERMES_GPUI_SOCKET` | `$XDG_RUNTIME_DIR/hermes-gpui-$USER.sock` | single-instance IPC path |
| `HERMES_OPEN_INSPECTOR` | unset | open the bot inspector at launch (`1`) |
| `HERMES_INSPECTOR_TAB` | `look` | `look`, `connectors`, `secrets` |
| `HERMES_HUB_URL` | unset | personal hub base URL; all three hub device variables must be set together |
| `HERMES_HUB_DEVICE_ID` | unset | device id returned by enrollment |
| `HERMES_HUB_DEVICE_TOKEN` | unset | device token; sent as a bearer header and never displayed |
| `HERMES_HUB_DEVICE_NAME` | `Hermes GPUI` | display name for this client device |
| `HERMES_OPEN_SETTINGS` | unset | open the sheet: `vault`, `model`, `assignments`, `skills`, `mcp`, `sessions`, `computers`, `usage` |
| `HERMES_DEMO` | unset | seed a short local transcript for screenshots |
| `RUST_LOG` | `hermes_desktop=info,hermes_api=info,hermes_rpc=info` | tracing filter |
| `HERMES_GPUI_LIVE_HOST` / `_TOKEN` | unset | only for `--ignored` live tests; never production |

Credentials are never written to disk by the client. Debug output redacts
tokens, cookies and query strings.

## Running

```bash
# local host
./target/release/hermes-desktop

# remote host with basic auth
HERMES_URL=https://hermes.example.com \
HERMES_BASIC=user:pass \
HERMES_PROFILE=ops \
./target/release/hermes-desktop

# headless verification of a host (REST, WS, session creation)
HERMES_URL=https://hermes.example.com HERMES_BASIC=user:pass \
  ./target/release/hermes-desktop --probe
```

Exit codes: `--probe` exits non-zero if any step fails and prints each step.

## Single instance

The first instance claims `$XDG_RUNTIME_DIR/hermes-gpui-$USER.sock`. A second
launch forwards **Show** (or **Toggle** with `--toggle`) and exits. If the socket
owner does not answer, the second launch exits with an error instead of opening a
duplicate window. On Omarchy, prefer `omarchy-launch-or-focus` for Hyprland
bindings — see [`OMARCHY.md`](OMARCHY.md).

Closing the window hides to the tray so a running turn keeps streaming; **Quit**
is on the tray menu and in the command palette.

## Personal hub and node agents

Phase 0 services live in `crates/hermes-hub` and `crates/hermes-node`. The hub
is single-owner and one replica; nodes dial out to it. Protocol details are in
[`NODE-PROTOCOL.md`](NODE-PROTOCOL.md).

### Hub

```bash
# local development
export HERMES_HUB_ADMIN_TOKEN="$(openssl rand -base64 48)"
HERMES_HUB_BIND=127.0.0.1:8080 HERMES_HUB_DB=/tmp/hermes-hub.db \
  cargo run -p hermes-hub

curl -fsS http://127.0.0.1:8080/readyz
```

| Variable | Default | Effect |
| --- | --- | --- |
| `HERMES_HUB_BIND` | `0.0.0.0:8080` | listen address |
| `HERMES_HUB_DB` | `data/hermes.db` | SQLite path; parent directories are created |
| `HERMES_HUB_ADMIN_TOKEN` | required | owner credential, at least 32 characters; never logged |
| `HERMES_HUB_HEARTBEAT_MS` | `15000` | heartbeat period advertised to nodes |
| `HERMES_HUB_MAX_COMMAND_TIMEOUT_MS` | `900000` | upper bound accepted by the API |

Mint an enrollment code, then exchange it once (the node token is printed only
by the second call):

```bash
curl -fsS -X POST http://127.0.0.1:8080/api/v1/enrollments \
  -H "Authorization: Bearer $HERMES_HUB_ADMIN_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"kind":"node","name":"forge"}'

curl -fsS -X POST http://127.0.0.1:8080/api/v1/enroll \
  -H 'content-type: application/json' -d '{"code":"<code>"}'
```

`GET /api/v1/nodes` shows presence and capabilities. `DELETE
/api/v1/nodes/{id}` revokes the node, closes its socket and fails its in-flight
commands. The same flow with `"kind":"device"` issues a client token; revoke it
with `DELETE /api/v1/devices/{id}`. `GET /api/v1/audit` is the personal audit;
it never contains a prompt value.

### Desktop client

Enroll this client once with a device code, then start the GPUI app with the
returned id and token together:

```bash
HERMES_HUB_URL=https://hermes \
HERMES_HUB_DEVICE_ID=device-… \
HERMES_HUB_DEVICE_TOKEN=… \
  ./target/release/hermes-desktop
```

`Ctrl+Shift+H` (or the **Computers** header button) shows the node roster,
presence, an argv-only command form, streamed output, a terminal panel on nodes
that advertise `pty`, and one-time credential cards. The client sends the token
only in the WebSocket `Authorization` header;
it reconnects with backoff and dequeues no offline commands. If only some hub
variables are set, the Computers page reports the configuration error instead
of silently falling back to gateway-only mode. The token header is never written
to logs, transcripts or the audit log.

### Kubernetes / ArgoCD

The chart is `charts/hermes-hub`. It expects the admin Secret
`hermes-hub-admin` with key `admin-token` in the target namespace and publishes
a Tailscale Ingress (`tailscale.hostname`, default `hermes`). Apply
`deploy/argocd/hermes-hub.yaml` after the Secret exists:

```bash
kubectl create namespace hermes
kubectl -n hermes create secret generic hermes-hub-admin \
  --from-literal=admin-token="$(openssl rand -base64 48)"
kubectl apply -f deploy/argocd/hermes-hub.yaml
kubectl -n hermes rollout status deploy/hermes-hub
```

The Application pins the image by digest in `helm.valuesObject` while the chart
keeps generic defaults. Build and publish a new image to the in-cluster
registry, then update that digest and let ArgoCD sync:

```bash
sudo docker build -f docker/hub.Dockerfile -t localhost:5000/hermes/hub:<tag> .
sudo docker push localhost:5000/hermes/hub:<tag>
# digest from `sudo docker inspect --format='{{index .RepoDigests 0}}' …`,
# then edit deploy/argocd/hermes-hub.yaml and push; ArgoCD applies it.
```

Do not expose the Service with a public LoadBalancer. The tailnet ACL limits the
network path; the hub bearer token remains the application gate.

Compose fallback: copy `deploy/compose/.env.example` to `.env`, set the token,
then `docker compose -f deploy/compose/compose.yaml up -d --build`. It binds
`127.0.0.1:8080` only.

### Node agent

Run `hermes-node` on each computer. It needs no inbound port.

| Variable | Default | Effect |
| --- | --- | --- |
| `HERMES_HUB_URL` | required | `http(s)://` or `ws(s)://`; normalised to `/v1/node` |
| `HERMES_NODE_TOKEN` | required | node token from enrollment |
| `HERMES_NODE_ID` | required | id returned by enrollment |
| `HERMES_NODE_NAME` | hostname | display name and prompt requester |
| `HERMES_NODE_ALLOWED_ROOTS` | `$HOME` | colon-separated existing directories; cwd must resolve inside one |
| `HERMES_NODE_ALLOW_COMMANDS` | empty | comma-separated program basenames; empty allows any program inside the roots |
| `HERMES_NODE_MAX_CONCURRENCY` | `2` | simultaneous children, clamped 1–16 |

Check effective policy without connecting:

```bash
HERMES_HUB_URL=https://hermes HERMES_NODE_ID=node-… \
HERMES_NODE_TOKEN=… hermes-node --check
```

A user unit should keep the token in a `0600` env file, not in `ExecStart`:

```ini
# ~/.config/systemd/user/hermes-node.service
[Unit]
Description=Hermes node agent
After=network-online.target

[Service]
EnvironmentFile=%h/.config/hermes/node.env
ExecStart=%h/.local/bin/hermes-node
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
```

The agent clears the child environment: it does not inherit
`HERMES_NODE_TOKEN`, the hub URL or host secrets. Only `PATH`, `HOME`, `LANG`,
`TERM`, the request's explicit env and a prompted secret are passed. A command
is executed without a shell, inside an allowed root, with a timeout and
cancellation. On Unix the agent also advertises `pty` (see **Terminals** below);
other platforms deny terminal requests.

Container variant (own filesystem, not the host's):

```bash
docker run --rm --read-only --tmpfs /tmp \
  -e HERMES_HUB_URL=https://hermes \
  -e HERMES_NODE_ID=node-… -e HERMES_NODE_TOKEN=… \
  -v "$HOME/work:/home/hermes/work" \
  ghcr.io/reverb256/hermes-node:0.1.0
```

### Terminals

On a node that advertises `pty`, **Open terminal** in Computers runs the entered
argv on a pseudo-terminal on that computer. The panel owns the keyboard while
it is focused: keys, arrows, Tab completion and Ctrl+C reach the remote process
as bytes, and the remote terminal echoes them back. `Ctrl+Shift+D` detaches the
keyboard (the session keeps running; click the panel to type again) and
`Ctrl+Shift+X` stops the session, which hangs up the process group.

The panel measures its own output area in prepaint and converts the pixel
bounds to a grid (about 7.2 × 15 px per cell at `text_xs`), clamped to 20–500
columns and 5–300 rows. The opening request carries the last measured grid, and
the client sends a `TerminalResize` frame whenever the measured grid changes,
so a resized window or a closed sheet reflows the remote program. The
conversion is approximate — the exact glyph advance depends on the font — but
it never repeats an unchanged frame and never resizes a finished session.

A terminal has no timeout, so `timeout_ms` must be zero with a `pty` spec. The
hub relays terminal bytes and stores none of them: they are absent from
`command_events`, the audit log and the transcript. Terminal records also stay
off the command output list. Treat the device token as local shell access,
because that is what it grants on every enrolled node.

### Secret prompt cards

A command that needs a credential carries `secret{env_var,purpose,scope,
timeout_ms}`. The node sends a prompt with a one-time challenge; every connected
device receives it. The first matching answer is forwarded once, injected into
the child environment and zeroized. A wrong challenge burns the prompt, and a
disconnect, expiry or node cancel closes it. With no device connected the hub
cancels immediately. The value is never in the audit, command record, argv,
transcript or node logs.

## Packaging and install status

`make release` produces `target/release/hermes-desktop`. An Arch PKGBUILD is
checked in at `packaging/PKGBUILD` and builds from that directory:

```bash
cd packaging && makepkg -si
```

It installs the binary, `.desktop` entry and Omarchy bar widget under
`/usr/share/hermes-gpui/`. There is still no signed package, auto-update channel
or crash reporting; those remain Phase 10 in [`../ROADMAP.md`](../ROADMAP.md).
The GitHub release workflow publishes an unsigned Linux x86_64 tarball plus
`SHA256SUMS`. To run from a download instead of a package, unpack it and put
`hermes-desktop` where your local tools live.

A user unit is optional; if you write one, keep it in the user manager and pass
credentials through `Environment=` rather than the unit's `ExecStart`:

```ini
# ~/.config/systemd/user/hermes-desktop.service (example, not shipped)
[Unit]
Description=Hermes GPUI
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Environment=HERMES_URL=https://hermes.example.com
EnvironmentFile=%h/.config/hermes/env
ExecStart=%h/.local/bin/hermes-desktop
Restart=on-failure

[Install]
WantedBy=graphical-session.target
```

Keep the env file mode `0600`. The client is a GUI app; `Restart=on-failure`
only covers crashes, not a closed window (closing hides to tray).

## Verification gates

| Gate | Command | What it proves |
| --- | --- | --- |
| Format | `cargo fmt --all --check` | style |
| Lint | `cargo clippy --workspace --all-targets --locked -- -D warnings` | no warnings |
| Tests | `cargo test --workspace --all-targets --locked` | unit + contract behaviour |
| Host probe | `hermes-desktop --probe` | REST, WS handshake, session creation |
| Live plugin | `HERMES_GPUI_LIVE_HOST=… HERMES_GPUI_LIVE_TOKEN=… cargo test -p hermes-api -- --ignored live_kanban` | real Kanban detail/comment/attachment/board behaviour and board isolation |
| Protocol | `cargo test -p hermes-protocol` | frame versioning, validation and secret redaction |
| Hub/node e2e | `cargo test -p hermes-hub --test e2e` | real enrollment, node execution, streams, prompt answer, audit and revoke |
| Chart | `helm lint charts/hermes-hub && helm template hermes-hub charts/hermes-hub --namespace hermes` | chart schema and rendered objects |
| OCI | `docker build -f docker/hermes-hub.Dockerfile .` | image build; requires a Docker daemon |

Record which gates were run when reporting a change; "compiles" is not a gate.

## Troubleshooting

| Symptom | Likely cause | Check |
| --- | --- | --- |
| Window opens, chat fails | gateway down or wrong URL | `--probe`; `GET /api/health` |
| 401/403 on a gated host | basic auth needs the cookie + WS ticket flow | `--probe` step output; `HERMES_TOKEN`/`HERMES_COOKIE` |
| WS disconnects every ~45 s | keepalive blocked (proxy idle timeout) | proxy config; `gateway.ping` in logs |
| Kanban empty with an error line | plugin not installed/enabled, or board missing | host plugin log; the error text is shown verbatim |
| Image shows “Load image” and fails | path not host-local/allowed | `/api/media?path=` response |
| Speak does nothing | no `HERMES_TTS_URL`, service down, or no player | `command -v pw-play paplay aplay`; service health |
| Dictation types nowhere | Superwhisper shortcut/paste target | `superwhisper status`; see [`OMARCHY.md`](OMARCHY.md) |
| Second launch opens nothing | stale socket or unresponsive primary | remove the socket only when no instance runs; `HERMES_GPUI_SOCKET` to isolate |
| Theme does not follow | Omarchy palette path changed or the file is missing | check `~/.local/state/omarchy/current/theme/colors.toml`; see [`OMARCHY.md`](OMARCHY.md) |
| Blank/black window on Wayland | GPU/driver or compositor mismatch | run under X11 to compare; `RUST_LOG=debug` |
| Node never appears online | bad token, wrong URL or TLS failure | `hermes-node --check`; hub logs; `curl /api/v1/nodes` |
| Command returns 409 | node offline or revoked | `GET /api/v1/nodes`; re-enroll if revoked |
| Prompt never appears | no device connected or challenge expired | `GET /api/v1/prompts`; device socket logs |
| `decoding` / `bad_frame` on connect | client and hub protocol versions differ | `GET /api/v1/health` and the agent version |
| Chart pod CrashLoopBackOff | admin Secret missing or wrong key | `kubectl -n hermes describe pod`; Secret name/key |

Logs go to stderr; run from a terminal or capture the journal when using a user
unit. The client never logs credentials.

## Backups and data

The client stores no user data beyond in-memory state and the IPC socket. All
sessions, profiles, secrets and Kanban data live on the host; back those up with
the host's own tooling (`/api/ops/backup` and its docs). Theme data belongs to
Omarchy.

The hub stores its SQLite database on the `/data` PVC (or the compose volume).
Take a PVC volume snapshot, or run SQLite's `.backup` from a temporary pod that
mounts the volume; the image does not ship the `sqlite3` CLI. Do not copy
`hermes.db` alone while the hub is writing: WAL mode also keeps `-wal` and
`-shm` files. The database contains command argv, stream chunks and audit; it
does not contain prompt values, command env or node tokens (only hashes).
Keep the admin token in a separate secret manager. A restart closes any
unresolved prompt as `hub_restarted`, because the one-time challenge lived in
the previous process.

## Security notes

- Prefer `HERMES_TOKEN` over basic auth where the host supports it; rotate tokens
  outside the client.
- The reveal endpoint is never called, so the client cannot read secrets back.
- Markdown never fetches remote content; images load only on explicit action.
- Report vulnerabilities privately per [`../SECURITY.md`](../SECURITY.md); never
  paste cookies or tokens into an issue.

## Omarchy integration commands

These are local to the desktop; none of them contact the host.

```bash
hermes-desktop status --json             # snapshot the bar widget reads
hermes-desktop --install-omarchy-hooks   # theme-set hook -> immediate repaint
hermes-desktop --diagnostics             # Omarchy version, theme, metrics, tools, paths
```

Screenshots are written under `$XDG_CACHE_HOME/hermes-gpui/screenshots`, so the
client can find the file it just captured without guessing `~/Pictures`. The bar
widget lives in `packaging/omarchy-shell-hermes` and is installed with
`omarchy plugin add <path>`; it reads the same `status.json` and needs no socket
access. Notifications go through `omarchy-notification-send`; without a shell
notification service the commands fail silently and the client keeps running.
