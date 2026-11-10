# 0004. Sync: append-only transcripts with revisions, LWW settings

- **Status:** Accepted
- **Date:** 2026-11-10
- **Deciders:** platform, client

## Context

Sessions currently live in a per-host SQLite database. Grok Bot keeps the same
bots, conversations and drafts across desktop and phone, and lets work continue
with the client closed. A general CRDT for every field would be expensive and
hard to reason about; a naive last-write-wins on transcripts would destroy
messages.

## Decision

- **Transcripts are append-only** and synced as ordered entries with a per-session
  sequence number and idempotency key; clients apply by sequence and ignore
  duplicates. Edits and deletions are new entries (tombstones), never rewrites.
- **Settings, drafts and UI state use last-writer-wins** with a server timestamp
  and a client revision; the loser is discarded and surfaced as a conflict only
  when it is user-visible (e.g. a renamed bot).
- **Blobs** (exports, images, artifacts) live in object storage referenced by
  content hash; the control plane stores only metadata.
- Streaming is resumable from the last applied sequence, so a dropped connection
  replays deltas instead of refetching the turn.

## Consequences

- Server-side ordering is authoritative; client clocks never decide transcript
  order.
- Storage grows monotonically per session; retention and compaction are explicit
  jobs, not implicit rewrites.
- Draft conflicts are possible but rare and visible; transcripts cannot conflict.
- The host must emit sequence numbers and idempotency keys for every event.
