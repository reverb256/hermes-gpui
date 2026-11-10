# Documentation index

| Document | Audience | What it covers | Status |
| --- | --- | --- | --- |
| [`../README.md`](../README.md) | users | install, run, connect, features quick tour, system requirements | current |
| [`../ROADMAP.md`](../ROADMAP.md) | everyone | phases, workstreams, client track, milestones, status rules | planning |
| [`../PARITY.md`](../PARITY.md) | everyone | per-feature status against Hermes Desktop and Grok Bot | living |
| [`FULL-PARITY-PLAN.md`](FULL-PARITY-PLAN.md) | architects | what must be built beyond a port: tenancy, cloud computers, collaboration, connectors, sync, governance, metering, distribution; decisions, verification, risks | planning |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | engineers | three layers, crates, protocols, auth flow, data flow, failure modes | current |
| [`FEATURES.md`](FEATURES.md) | users, support | per-surface behaviour and the exact host routes behind it | current |
| [`DEVELOPMENT.md`](DEVELOPMENT.md) | contributors | toolchain, build, checks, test strategy, disposable host harness, commit rules | current |
| [`OPERATIONS.md`](OPERATIONS.md) | operators | environment variables, host prerequisites, packaging, troubleshooting, verification gates | current |
| [`OMARCHY.md`](OMARCHY.md) | users, contributors | Omarchy 4.x integration: theme, notifications, bar plugin, launch/focus, capture, Superwhisper dictation, packaging | current |
| [`adr/`](adr/README.md) | architects | decision records for the layer split, the shared computer boundary, connector brokering and sync | accepted |
| [`../CONTRIBUTING.md`](../CONTRIBUTING.md) | contributors | PR flow and review expectations | current |
| [`../STYLE.md`](../STYLE.md) | contributors | UI tokens, spacing, contrast rules | current |
| [`../SECURITY.md`](../SECURITY.md) | security | private vulnerability reporting | current |
| [`../CHANGELOG.md`](../CHANGELOG.md) | users | released behaviour changes | current |

## Conventions

- Every capability gets a row in `PARITY.md`; every non-port capability gets a
  workstream in `FULL-PARITY-PLAN.md` and a phase here.
- Status language is literal: “Implemented” means client code exists;
  “verified” means a test or live run is linked in the same change.
- Documents that describe behaviour must name the wire surface they rely on, so
  a host upgrade can be diffed against them.
- Plans carry a date and a status banner. A stale plan is corrected, not
  silently extended.

## Adding documentation

1. Behaviour changes update `FEATURES.md` and `CHANGELOG.md`.
2. Status changes update `PARITY.md`; phase changes update `ROADMAP.md`.
3. A new architectural decision gets an ADR in `adr/` (copy
   [`adr/template.md`](adr/template.md)).
4. New operator-visible configuration goes in `OPERATIONS.md` with its default
   and effect.
