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

The client defaults to `http://127.0.0.1:9119` (loopback dashboard).

## Environment variables

| Variable | Default | Effect |
| --- | --- | --- |
| `HERMES_URL` | `http://127.0.0.1:9119` | host origin; scheme/host/port only |
| `HERMES_TOKEN` | unset | bearer token for every REST call and WS query |
| `HERMES_BASIC` | unset | `user:pass`; gated hosts trade it for a cookie + WS ticket |
| `HERMES_COOKIE` | unset | ready-made `__Host-hermes_session_at=…`; skips login |
| `HERMES_PROFILE` | host active profile | profile the chat is scoped to |
| `HERMES_TTS_URL` | unset | base URL of a `voice-models` service (`POST /tts`) |
| `HERMES_TTS_VOICE` | `en-US-AndrewNeural` | TTS voice name |
| `HERMES_GPUI_SOCKET` | `$XDG_RUNTIME_DIR/hermes-gpui-$USER.sock` | single-instance IPC path |
| `HERMES_OPEN_INSPECTOR` | unset | open the bot inspector at launch (`1`) |
| `HERMES_INSPECTOR_TAB` | `look` | `look`, `connectors`, `secrets` |
| `HERMES_OPEN_SETTINGS` | unset | open the sheet: `vault`, `model`, `assignments`, `skills`, `mcp`, `sessions`, `usage` |
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

Logs go to stderr; run from a terminal or capture the journal when using a user
unit. The client never logs credentials.

## Backups and data

The client stores no user data beyond in-memory state and the IPC socket. All
sessions, profiles, secrets and Kanban data live on the host; back those up with
the host's own tooling (`/api/ops/backup` and its docs). Theme data belongs to
Omarchy.

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
