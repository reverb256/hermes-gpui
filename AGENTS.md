# AGENTS.md — hermes-gpui

Native GPUI desktop client for Hermes (Rust workspace under `crates/`).

Standing rules for every agent working here (Grok Bot fleet, Hermes profiles, OpenHands workers). Short on purpose; keep it accurate.

## Owner

- Owner bot: **Dev**. The owner reviews and gates merges; other agents open PRs.
- Default branch: `main`.

## Build and test

```bash
make check              # cargo fmt --check, clippy -D warnings, cargo test (what CI runs)
```

- See CONTRIBUTING.md and STYLE.md. Tests that need a live Hermes host stay out of automated tests.

## Merging

- Never push to `main`. Work on a branch and open a PR.
- There is no label gate here: the owner squash-merges after CI is green. Agents never run `gh pr merge` and never add labels.
- No force-push, rebase, amend or `--no-verify`. If a PR is behind, merge `main` into the branch.
- Builds and tests run in CI or on the designated build host (niced, CPU only), never on the git-only or miner hosts.

## Secrets and personal details

- No secrets, tokens, keys, `.env` files, databases or tool/session state (`.hermes/`, `.cursor/`, `.claude/` and the like) in any commit. Stage explicit paths; never `git add -A` or `git add .`.
- No personal details: real names, addresses, city, phone numbers, or home network layout. This repo is **public**: treat every line as published.
- Commit as `Reverb <129622142+reverb256@users.noreply.github.com>`.
- A leakguard block is a finding to report, never something to work around.
