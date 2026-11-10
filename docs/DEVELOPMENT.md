# Development

**Last updated:** 2026-11-10. See [`../CONTRIBUTING.md`](../CONTRIBUTING.md) for
the contribution flow and [`../STYLE.md`](../STYLE.md) for UI rules.

## Toolchain and prerequisites

- Rust is pinned by `rust-toolchain.toml` (workspace `rust-version = "1.95"`,
  edition 2024); rustup installs `rustfmt` and `clippy` automatically.
- Linux build dependencies: `wayland` or `x11`, `libxkbcommon`, `vulkan-icd-loader`,
  `fontconfig`, `freetype2`, plus `pkg-config`/`clang` for bindgen-heavy crates.
  On Arch: `pacman -S base-devel wayland libxkbcommon vulkan-icd-loader
  vulkan-headers fontconfig freetype2`.
- CI installs the equivalent Debian packages; see
  [`.github/workflows/ci.yml`](../.github/workflows/ci.yml).

## Build and run

```bash
make run            # cargo run -p hermes-desktop
make release        # cargo build --release --locked -p hermes-desktop
./target/debug/hermes-desktop --probe   # headless host check
```

The app reads its host from `HERMES_URL`/`HERMES_TOKEN`/`HERMES_BASIC`/
`HERMES_COOKIE` (see [`OPERATIONS.md`](OPERATIONS.md)).

## Checks

```bash
make check   # fmt --check, clippy -D warnings, tests; mirrors CI
```

Individually: `make fmt`, `make clippy`, `make test`. CI runs the same commands
with `--locked` on every push and pull request. A change is not ready until
`make check` is green locally.

## Test strategy

| Layer | Where | What it proves |
| --- | --- | --- |
| Pure unit tests | next to the code (`theme.rs`, `transcript.rs`, `sessions.rs`, `backend.rs`, `hermes-api`) | parsing, filtering, merge rules, offline behaviour, request shapes |
| Local contract tests | `crates/hermes-api/src/lib.rs` | exact HTTP method, path, query encoding, headers and bodies against a throwaway `TcpListener` |
| Live host tests | `#[ignore]` in `hermes-api`, run explicitly | real plugin/host behaviour, including board scoping |
| Manual QA | `--probe`, `HERMES_DEMO=1`, QA env hooks | handshake, rendering, screenshot review on an isolated display |

### Live test against a disposable host

Never point these at a production host: they create boards, tasks and files.

```bash
# 1. disposable python env + home (already provisioned in target/)
python -m venv target/disposable-hermes-venv   # if missing
# install the host's web extra from ../hermes-agent per its pyproject

# 2. start the host
HOME=$PWD/target/disposable-user \
XDG_CONFIG_HOME=$PWD/target/disposable-user/.config \
XDG_CACHE_HOME=$PWD/target/disposable-user/.cache \
HERMES_HOME=$PWD/target/disposable-hermes-home \
PYTHONPATH=$PWD/../hermes-agent \
HERMES_DASHBOARD_SESSION_TOKEN=disposable-verify-token \
target/disposable-hermes-venv/bin/python -m hermes_cli.main serve \
  --host 127.0.0.1 --port 0 --isolated

# 3. read the port from the log line HERMES_BACKEND_READY port=NNNN, then
HERMES_GPUI_LIVE_HOST=http://127.0.0.1:NNNN \
HERMES_GPUI_LIVE_TOKEN=disposable-verify-token \
cargo test -p hermes-api --locked -- --ignored live_kanban
```

`target/` is disposable and ignored by git. Kill the host by PID when done; do
not use a `pkill -f` pattern that also matches the shell running it.

## Code layout and rules

- `hermes-rpc` stays transport-only; no product logic.
- `hermes-api` holds every wire call. New routes need a typed struct plus a
  contract test. Optional features must degrade to a visible error, not an empty
  state.
- `hermes-desktop` holds UI state; anything testable without a window goes in a
  helper module with unit tests.
- Network work happens in `backend.rs`'s worker, never on the UI thread.
- Failures are surfaced to the user and never replayed silently.
- Match existing style; there is no ESLint/Prettier equivalent here, so
  `cargo fmt` plus the surrounding code is the standard.

## Documentation rules

- Behaviour changes update [`FEATURES.md`](FEATURES.md) and `CHANGELOG.md`.
- Status changes update [`../PARITY.md`](../PARITY.md); phase changes update
  [`../ROADMAP.md`](../ROADMAP.md).
- New architectural decisions get an ADR in [`adr/`](adr/README.md).
- New configuration is documented in [`OPERATIONS.md`](OPERATIONS.md) with its
  default and effect.

## Commits and pull requests

- One concern per commit; imperative subject; the body explains *why*.
- Reference the issue in the commit message with `Closes #NNN` — a squash merge
  can drop it from the PR body.
- Update the docs above in the same commit as the behaviour.
- Do not claim verification that was not run; state what was checked and what
  was not.

## Optional tools used in this workspace

`mise` (tool versions), `gh` (GitHub), `kubectl` (cluster), and a disposable
Hermes environment under `target/`. None are required to build the client.
