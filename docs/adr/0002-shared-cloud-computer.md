# 0002. One cloud computer per user, shared by that user's bots

- **Status:** Accepted
- **Date:** 2026-11-10
- **Deciders:** platform, security

## Context

Grok Bot gives every user one persistent cloud computer with a browser,
filesystem and terminal; every bot on the account shares it, so handoffs work
without repeating logins or file setup. Isolation is between users, not between
bots. Hermes `computer_use` currently drives a display on the machine the agent
runs on, with no provisioning, snapshots or screen streaming.

Two models are possible: a sandbox per bot (stronger isolation, no shared
logins/handoffs) or a sandbox per user (matches the reference product, weaker
boundary between bots).

## Decision

Provision **one sandbox per user**, shared by that user's bots, with **per-bot
virtual screens** and one computer-use task per screen at a time. State the
boundary explicitly in the UI and docs: anything placed on the computer is
available to every bot the user runs. Offer a stronger per-bot tier later for
customers who need it; do not pretend the shared model is a security boundary.

## Consequences

- Handoffs, shared cookies and files work as in Grok Bot; onboarding is simpler.
- A malicious or mistaken bot can read another bot's files — mitigated by
  approvals, audit, per-bot screens and clear warnings, not by isolation.
- Snapshot/restore is per user, which keeps the model small.
- Cross-user isolation must be enforced at provisioning, tokens, network and
  storage layers, and tested as a matrix.
