# Contributing

Thanks for helping improve hermes-gpui. This is a native GPUI client; Electron
and Chromium are out of scope.

## Prerequisites

- Rust (the pinned version in `rust-toolchain.toml`; rustup installs it
  automatically).
- Linux build libraries: `wayland`/`libxkbcommon`, X11/XCB, Vulkan loader,
  `fontconfig`, `freetype2`, and `dbus`. `README.md` has the Arch package list;
  the CI workflow lists the Debian/Ubuntu equivalents.
- A reachable Hermes host for manual testing. Never commit credentials: the
  client reads `HERMES_URL`, `HERMES_TOKEN`, `HERMES_BASIC`, `HERMES_COOKIE` and
  `HERMES_PROFILE` from the environment.

## Commands

```bash
make check          # fmt + clippy + tests, exactly what CI runs
cargo run -p hermes-desktop
```

The individual commands are `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace --all-targets`. All three run in CI and must pass. Add tests for new pure logic (mapping,
filtering, formatting, token invariants); UI-only changes still need a manual
run and a note in the pull request.

## Pull requests

- Keep changes focused. No drive-by refactors, no stubs for unavailable
  surfaces.
- Write a descriptive commit subject and put `Closes #NNN` in the commit
  message, not only the PR body (squash merges drop the body).
- Follow `STYLE.md` for UI work and `PARITY.md` for remaining host surfaces.
- Note any live host mutation (toggling a skill, enabling an MCP server) in the
  testing section and keep it out of automated tests.
- By contributing you agree that your work is licensed under the repository's
  MIT license.

## Releases

1. Update `CHANGELOG.md` and move the entries out of `[Unreleased]`.
2. Bump `version` in the root `Cargo.toml` and refresh `Cargo.lock`.
3. Commit, then tag `vX.Y.Z` and push the tag.
4. The `Release` workflow builds the Linux x86_64 tarball, writes
   `SHA256SUMS`, and creates the GitHub release with generated notes.

## Security

Do not open a public issue for a vulnerability. Follow `SECURITY.md`.

## Conduct

Participation is covered by `CODE_OF_CONDUCT.md`.
