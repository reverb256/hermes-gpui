# Feature reference

**Last updated:** 2026-11-10. Behaviour is described as implemented in this
repository. The wire surface column names what the client actually calls, so a
host upgrade can be diffed against this page. Status against the two reference
products is in [`../PARITY.md`](../PARITY.md).

## Chat

| Capability | Behaviour | Wire surface |
| --- | --- | --- |
| Submit a prompt | Composer is a native GPUI multiline editor (selection, cursor, undo, IME) | `prompt.submit` |
| Streaming | Assistant deltas merge into the trailing line of the same role; reasoning is hoisted above the answer | `message.delta`, reasoning deltas |
| Interrupt | Header stop button interrupts the active turn | interrupt RPC |
| Approvals | Cards offer allow once / always allow / deny; clarify cards collect an answer | approval + clarify RPC |
| Mentions | `@` offers bots and skills | client-side over loaded rosters |
| Attachments | File and image pickers upload to the host | host upload routes |
| Markdown | CommonMark blocks (paragraph, heading, code, quote, list, rule) and inline bold/italic/code/link styling; links are styled text, not fetched | `transcript::markdown_blocks` |
| Inline images | Standalone host-local PNG/JPEG/WebP references offer **Load image** | `/api/media?path=` |
| Copy reply | Each assistant reply has a **Copy** action that writes the full text to the clipboard | client-side |
| Find in transcript | `Ctrl+F` opens a find bar over the loaded lines, highlights matches and cycles them with Enter / Shift+Enter; Esc closes | `transcript::find_lines` |
| Speak | Reads the last reply through the external TTS service and plays it locally | `POST {HERMES_TTS_URL}/tts`, `pw-play`/`paplay`/`aplay` |
| Session switch | `Ctrl+1…9`, sidebar bots, or history rail | `session.resume` |

Limits: search covers only the lines already loaded (no server-wide or
virtualized search), and there is no message selection, retry/fork or syntax
highlighting. Remote content is never fetched by design.

## Sessions

| Capability | Behaviour | Wire surface |
| --- | --- | --- |
| History list | Newest first, message/tool counts and relative age, client-side search and project filter | `/api/sessions?limit=50&offset=N&min_messages=1&archived=exclude&order=recent` |
| Paging | **Load more** appends the next 50, de-duplicating rows that shifted between requests; footer shows loaded of total | same, `offset` |
| Resume | Requests `session.resume`; older hosts fall back to a read-only transcript with an explanation | `session.resume`, `/api/sessions/{id}/messages` |
| Rename / pin / archive / delete | Inline rename, pin toggle, archive, confirmed delete | host session update/delete routes |
| Export | Saves the JSON export to a user-chosen path | host export route |

## Profiles and bots

| Capability | Behaviour | Wire surface |
| --- | --- | --- |
| Roster | Sidebar with last-message previews, search, pin state, switching | `profiles.list`, `/api/profiles` |
| Look | Icon glyph, accent colour, avatar (PNG/JPEG/WebP ≤ 2 MB) shared by every client | `profiles.configure`, `profiles.set_asset`, `profiles.get_asset` |
| Connectors | Per-bot MCP servers and toolsets toggles | `profiles.describe`, `profiles.configure` |
| Secrets | List, add, replace, delete host env keys; values stay redacted | `GET/PUT/DELETE /api/env` |
| Mentions | `@` completion over bots and skills | client-side |

Not implemented: creating/duplicating/deleting bots, bot sections, shared bots.

## Settings: model, assignments, vault, usage

| Surface | Behaviour | Wire surface |
| --- | --- | --- |
| Model | Searchable provider/model list; picking applies to new sessions only | `/api/model/options`, `POST /api/model/set` |
| Assignments | Helper tasks pinned to `provider · model` or “Follows main”, plus reasoning level | `/api/model/auxiliary`, `POST /api/model/set` (`scope: auxiliary`), `/api/config` |
| Vault | Credential catalog by host category, search, per-entry replace, new item | `/api/env` |
| Usage & Billing | 7/30/90-day totals, per-model spend, running model and context, plan tier and billing link | `/api/analytics/usage`, `/api/model/info`, `/api/portal` |

Billing mutations are not performed; the page reports and links out. A host with
an unreadable `state.db` returns 503 and that explanation is shown.

## MCP and skills

| Capability | Behaviour | Wire surface |
| --- | --- | --- |
| Server list | Transport, source, endpoint, auth state | `/api/mcp/servers` |
| Test | Host-side ping | `POST /api/mcp/servers/{name}/test` |
| Enable/disable | Toggle per server | `PUT /api/mcp/servers/{name}/enabled` |
| Add / edit | Validated HTTP/stdio form; edit saves the whole map, preserving tool selection and unknown keys | `POST /api/mcp/servers`, `PUT /api/mcp/servers` |
| Delete | Two-click confirmation | `DELETE /api/mcp/servers/{name}` |
| Catalog | Curated entries, required env prompts, install progress | catalog routes |
| OAuth | Host-managed start/status/cancel with browser hand-off | `/api/mcp/oauth/flows*` |
| Skills | Installed list with search, categories, usage counts, On/Off | `/api/skills`, `PUT /api/skills/toggle` |
| Skill hub | Search, preview real `SKILL.md`, security scan, confirmed install | hub search/preview/install routes |

Limits: per-tool selection and cost hints are not surfaced; live OAuth provider
behaviour is not verified.

## Automations (cron) and MoA

| Capability | Behaviour | Wire surface |
| --- | --- | --- |
| Cron list | Jobs with schedule and state | `/api/cron/jobs` |
| Create | Prompt, schedule and delivery target | `POST /api/cron/jobs`, `/api/cron/delivery-targets` |
| Pause / resume / trigger | Per-job actions | `POST /api/cron/jobs/{id}/{action}` |
| Delete | Confirmed | `DELETE /api/cron/jobs/{id}` |
| Run history | Recent runs as sessions | `/api/cron/jobs/{id}/runs` |
| MoA | Edits existing presets, preserving the full preset map on save | MoA config routes |

Not implemented: event triggers, richer schedule editing, timezone controls.

## Memory

| Capability | Behaviour | Wire surface |
| --- | --- | --- |
| Providers | Discovered providers with readiness | `/api/memory`, `/api/memory/providers` |
| Configure | Schema-driven form (text, secret, boolean, number, select, JSON, conditional fields) | `/api/memory/providers/{name}/config` |
| Activate | Only host-ready providers are selectable | `PUT /api/memory/provider` |
| Reset | Built-in memory files reset behind a second confirming click | `POST /api/memory/reset` |

## Kanban (optional plugin)

Board selection is client-local; every request carries `?board=<slug>`, so the
host's global active board is never changed.

| Capability | Behaviour | Wire surface |
| --- | --- | --- |
| Boards | List, local selection, create (no switch), display-name rename, confirmed archive, confirmed hard delete | `/api/plugins/kanban/boards` (`POST`, `PATCH /{slug}`, `DELETE /{slug}?delete=`) |
| Board read | Columns and tasks for the selected board | `/api/plugins/kanban/board?board=` |
| Task create | Title only, starts in triage, no assignee, no dispatch | `POST /api/plugins/kanban/tasks?board=` body `{title, triage:true}` |
| Task edit | Title edits; description, assignee (empty clears) and priority edited in the drawer | `PATCH /api/plugins/kanban/tasks/{id}?board=` |
| Status transitions | Host-validated “next” transitions only | same PATCH with `status` |
| Delete | Two-click confirmation | `DELETE /api/plugins/kanban/tasks/{id}?board=` |
| Task details | Description, assignee, summary, result, priority, comments, attachments | `GET /api/plugins/kanban/tasks/{id}?board=` |
| Comments | Add with paste, Enter to send, Shift+Enter for newline; draft kept on failure | `POST /api/plugins/kanban/tasks/{id}/comments?board=` |
| Attachments | Upload with picker, save to a chosen path, remove with confirmation; size and author shown | `POST …/attachments`, `GET /api/plugins/kanban/attachments/{id}`, `DELETE …/{id}` |

Not implemented: drag-and-drop, filters/search, bulk actions, live board events,
dispatch, model overrides, runs/diagnostics views, board description/icon/colour
and workdir settings.

## Desktop integration

| Capability | Behaviour |
| --- | --- |
| Single instance | Socket at `$XDG_RUNTIME_DIR/hermes-gpui-$USER.sock` (or `HERMES_GPUI_SOCKET`); second launch forwards Show/Toggle |
| Tray | Show Hermes, Quit; window close hides to tray so turns keep streaming |
| Theme | Follows Omarchy `colors.toml`, re-read at most every 2 s, semantic token layer, contrast/scale invariants unit-tested |
| Voice | External `voice-models` TTS; dictation stays with Superwhisper (`Alt+Space`) |
| QA hooks | `--probe`, `HERMES_DEMO`, `HERMES_OPEN_INSPECTOR`, `HERMES_INSPECTOR_TAB`, `HERMES_OPEN_SETTINGS` |

See [`OPERATIONS.md`](OPERATIONS.md) for every environment variable and
[`../PARITY.md`](../PARITY.md) for what is still missing (notifications, voice
capture, terminal, computer view, artifacts, multi-host).
