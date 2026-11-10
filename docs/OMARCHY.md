# Omarchy integration

**Last updated:** 2026-11-10. This host reports Omarchy `4.0.0.alpha` in
`~/.local/share/omarchy/version`; upstream announced 4.5 on 2026-11-04, so
command availability can differ by a release. Verify with
`omarchy commands --json` before relying on a command.

Omarchy 4.x is Arch + Hyprland + a single long-running Quickshell process,
`omarchy-shell`. The bar, panels, overlays, menus and notification daemon are
plugins inside that process. Hyprland configuration is Lua under
`~/.config/hypr/`. This shapes what a desktop app should integrate with.

## Current integration

| Capability | Mechanism used today | Where |
| --- | --- | --- |
| Theme | Reads Omarchy's `~/.local/state/omarchy/current/theme/colors.toml` only, maps it to semantic tokens, re-reads at most every 2 s | `crates/hermes-desktop/src/theme.rs` |
| Single instance | Own IPC socket + `--toggle`; second launch forwards Show/Toggle | `ipc.rs`, `main.rs` |
| Tray | `ksni` tray with Show/Quit; window close hides to tray so turns keep streaming | `tray.rs` |
| Audio playback | `pw-play` → `paplay` → `aplay`, first on `PATH` | `voice.rs` |
| File dialogs | `rfd` with `xdg-portal` (works on Wayland/Hyprland) | pickers in `app.rs` |
| Clipboard | GPUI clipboard (Wayland data device) | copy reply, paste in editors |
| Dictation | External Superwhisper; the app never captures audio | README |

## Implemented in the client

| Surface | Where | Behaviour |
| --- | --- | --- |
| Theme palette | `theme.rs` | `colors.toml` mapped onto semantic tokens, checked for contrast |
| Theme changes | `theme.rs`, `omarchy.rs` | 2 s poll of Omarchy's `colors.toml`; `--install-omarchy-hooks` drops a `theme-set` hook that forces an immediate reload |
| Notifications | `omarchy.rs` | turn completion and approvals via `omarchy-notification-send`, `--replace-id` collapse, click `--toggle`, “Remind in 15” action |
| Bar widget | `packaging/omarchy-shell-hermes/` | `bar-widget` plugin that reads `hermes-desktop status --json`; click toggles, middle click refreshes |
| Status channel | `status.rs` | atomic snapshot at `$XDG_CACHE_HOME/hermes-gpui/status.json` (connection, streaming, approvals, last reply) |
| Capture | `app.rs`, `omarchy.rs` | “▣ Shot” attaches `omarchy capture screenshot`; “T Text” OCRs with `omarchy capture text` into the composer |
| Launch | `ipc.rs`, `packaging/` | `--toggle` single instance, `.desktop` entry, `PKGBUILD` installing binary, entry and widget |
| Diagnostics | `main.rs` | `--diagnostics` prints Omarchy version, theme, metrics, tools, cache and socket paths |

Install the pieces:

```bash
hermes-desktop --install-omarchy-hooks      # theme-set hook
hermes-desktop status --json                # what the bar widget reads
hermes-desktop --diagnostics                # support information
install -d ~/.config/omarchy/plugins/io.github.hermes.gpui
cp packaging/omarchy-shell-hermes/* ~/.config/omarchy/plugins/io.github.hermes.gpui/
omarchy plugin validate ~/.config/omarchy/plugins/io.github.hermes.gpui
```

The widget is third-party Quickshell code, so it is unsandboxed and must be
enabled by the user (`omarchy plugin enable io.github.hermes.gpui`).

## Conventions to hook into (and what to change)

### 1. Launch and focus: use Omarchy's helper, not a custom socket

Omarchy ships `omarchy-launch-or-focus` (and `-or-focus-tui`,
`-or-focus-webapp`), which matches the window class **or title**
case-insensitively, focuses the first match with `hyprctl`, and otherwise runs
the command through `uwsm-app --`. That makes it the idiomatic Hyprland binding
and the right companion to our own `--toggle`.

```lua
-- ~/.config/hypr/bindings.lua  (Omarchy 4.x Lua config)
o.bind("SUPER + H", "Hermes", { launch = "hermes-desktop" })
-- or keep the built-in toggle behaviour:
o.bind("SUPER + SHIFT + H", "Hermes (toggle)", { launch = "hermes-desktop --toggle" })
```

Keep `--toggle` working for tray/other desktops; document the Omarchy binding in
the README. Set a stable Wayland `app_id` so `omarchy-hyprland-focus-app` and
window rules can target the window.

### 2. Notifications: `omarchy-notification-send`, never `notify-send`

The shell claims `org.freedesktop.Notifications`; `omarchy-notification-send`
respects DND, stacks cards, supports action buttons and takes an argv click
command. This is the correct channel for “turn finished”, “approval needed” and
routine results. Its full surface (read from `/usr/share/omarchy/bin/`):

| Flag | Meaning |
| --- | --- |
| `--app-name` | sender name, default `omarchy-action`; set `Hermes` |
| `-u/--urgency` | `low` (default), `normal`, `critical` |
| `-g/--glyph`, `-i/--icon`, `--image` | glyph, icon name, or image path/URI |
| `-t/--expire-time` | milliseconds; `-1` leaves it to the shell |
| `-r/--replace-id` | numeric id to replace — use it so repeated events collapse |
| `-p/--print-id` | print the assigned id, to pass back to `--replace-id` |
| `--transient` | do not persist in the notification center |
| `--exec <program> [args...]` | click command, argv words (no shell string) |
| `--action <label> <program> [words...]` | repeatable action button |
| `--with <args...>` | arguments appended to every action |

```bash
# first event: capture the id
ID=$(omarchy-notification-send -p --app-name Hermes -u normal \
  "Turn finished" "ops · 3 tools · 12 s" --exec hermes-desktop --toggle)
# later events: collapse into the same card
omarchy-notification-send -r "$ID" --app-name Hermes -u normal \
  "Approval needed" "kanban t_5f2882b5" --exec hermes-desktop --toggle
```

Next step for the client: emit a notification when a turn completes while the
window is hidden, route the click through `--toggle`, and give approvals two
`--action` buttons (Open, Allow) that map to argv commands. `Super + Comma`
acts on the newest card, so keeping one card per event type with `--replace-id`
is better than stacking duplicates.

### 3. Bar widget: an `omarchy-shell` plugin, not a tray icon

Omarchy 4.x has no separate Waybar system; a bar widget is a Quickshell plugin
with a `manifest.json` (`id`, `kinds: ["bar-widget"]`, `entryPoints.barWidget`,
optional `barWidget.defaultSection`) loaded by `omarchy-shell`. Placement is
managed with `omarchy bar move <id> --section left|center|right` and enabling
with `omarchy plugin enable <id>`.

The default bar already ships an `omarchy.agents` widget, which is the natural
neighbour for agent work — check whether contributing a row to it is cheaper than
a second widget before adding one. Plugin tooling: `omarchy plugin list` (ids,
enabled state, first/third-party), `add` (git, lands disabled), `clone` (built-in
to `~/.config/omarchy/plugins/`), `validate <dir>` against the manifest schema,
`update`, `remove`. Remove or validate before enabling; plugins are unsandboxed
Quickshell code.

The widget shows connection state (green/amber/red), a streaming indicator and
the pending approval count; left click toggles the window. A widget cannot read
the client's memory, so it uses a small state channel — options, in preference
order:

1. A Unix socket in the runtime dir that the widget queries (mirrors the IPC
   socket; no new daemon).
2. A `hermes status --json` CLI subcommand the widget shells out to.
3. A shell `service` plugin that subscribes to the same socket.

Ship it as a plugin users copy into `~/.config/omarchy/plugins/` and enable
(plugins are unsandboxed; document that warning).

### 4. Theme changes: prefer the `theme-set` hook over polling

Omarchy hooks run from `~/.config/omarchy/hooks/<name>` and
`~/.config/omarchy/hooks/<name>.d/`; `omarchy hook install <type> <file>` puts a
script in the `.d` directory. `omarchy-theme-set` takes a lock at
`$XDG_RUNTIME_DIR/omarchy-theme-set.lock` while switching and logs to
`~/.local/state/omarchy/theme-set.log`.

The client reads one source: `~/.local/state/omarchy/current/theme/colors.toml`.
There is no separate theme, no override and no `shell.toml`/`theme.name`
ingestion; other desktops get the bundled fallback palette. Polling every 2 s is
the default; the `theme-set` hook touches a marker that forces an immediate
reload, so a theme switch does not wait for the next tick.

### 5. Screenshots and capture: reuse `omarchy capture`

`omarchy capture screenshot`, `capture screenrecording`, `capture text` (OCR of a
region) and `capture qr` are the desktop's capture path. Instead of building a
capture pipeline:

- “Attach screenshot” invokes `omarchy capture screenshot` and feeds the resulting
  path into the existing attachment upload.
- “Explain this screen” runs `omarchy capture text` and puts the OCR result in
  the composer for review.

This is the cheapest path to visual bug reports and to attaching UI context to a
prompt, and it stays consistent with the desktop's own shortcuts.

### 6. Dictation: Superwhisper is the default, so don't compete

Superwhisper runs as `superwhisper.service` with a `superwhisper` CLI, default
`Alt + Space` toggle and auto-paste into the focused window. On Omarchy it also
has a bar panel (`omarchy-shell superwhisper open|page language|microphone`).

Client rules:

- Keep the composer focusable by a single click; never grab the keyboard while
  Superwhisper records.
- Treat dictation as pasted text: `Ctrl+V` and Superwhisper's text commit both
  land in the composer. Do not add an in-app recorder.
- If a “dictate” affordance is added, it should focus the composer and show the
  configured shortcut, not simulate it. Read the shortcut from
  `superwhisper shortcuts show` when available.
- Offline modes are large (≈2.2 GB) and `linux.autoUnloadModels` is on; do not
  assume the service is always warm.
- Troubleshooting starts with `superwhisper status`, then
  `systemctl --user status superwhisper`.

### 7. Idle, lock and screensaver

Idle and lock are shell features configured in `~/.config/omarchy/shell.json`
(`idle.lock` seconds). A running turn must keep streaming while the session is
locked; the client already hides to tray. Notify on completion after unlock
rather than fighting the lock screen. Never disable idle or lock from the app.

### 8. Packaging and menu entry

Omarchy installs system packages with `omarchy pkg add` (AUR via
`omarchy pkg aur add`). A future package should:

- install the binary to `/usr/bin` or `/usr/local/bin`,
- ship a `hermes-desktop.desktop` with `StartupWMClass` set to the Wayland
  `app_id`, an icon in `hicolor`,
- avoid a systemd *system* unit; this is a session app,
- document the `SUPER + H` binding rather than editing `bindings.lua` itself.

The Omarchy menu is configurable through
`~/.config/omarchy/extensions/omarchy-menu.jsonc` (hot-reloads); a `.desktop`
entry makes the app discoverable without editing that file.

### 9. Reminders and other surfaces worth reusing

- `omarchy reminder <minutes> [message]` writes a desktop reminder; a routine
  result could offer “remind me in 15” instead of inventing a scheduler UI.
- `omarchy menu input` / `omarchy menu select` give the desktop's own prompt
  widgets to shell scripts; useful for a small helper CLI if the app ever needs
  one.
- `omarchy commands --json` is the machine-readable command catalogue; use it in
  scripts instead of hardcoding command availability.

### 10. Diagnostics

`omarchy debug --no-sudo --print` gathers system state without prompting; also
`omarchy version` and `omarchy commands --json`.
Pair it with `RUST_LOG=debug hermes-desktop` and `journalctl --user` when
reporting a desktop-level issue. Never include tokens or cookies.

## What deliberately does not integrate

- No Quickshell import of app state; the shell is a separate process.
- No Hyprland socket control from the client (no focus stealing, no workspace
  moves); use the documented Omarchy helpers.
- No modification of `/usr/share/omarchy/`; user configuration stays in
  `~/.config/`.
- No bundled audio capture, TTS model or dictation model.

## Verification for Omarchy work

1. `omarchy version`, `omarchy commands --json` — confirm the commands used.
2. Theme: switch with `omarchy theme set <name>` and confirm the window updates
   within 2 s (or via the hook) without restart.
3. Notifications: trigger a hidden-window completion; confirm the card appears,
   respects DND, and the click toggles the window.
4. Bar plugin: `omarchy plugin list`, enable in a scratch shell config, confirm
   it does not break `omarchy refresh shell` defaults.
5. Dictation: with the composer focused, `Alt + Space`, speak, confirm the text
   lands at the cursor and `Ctrl+Z` undoes it as one edit.
6. Record results in [`../PARITY.md`](../PARITY.md) before claiming support.
