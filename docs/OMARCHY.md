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
| Theme | Reads `~/.local/state/omarchy/current/theme/colors.toml` (fallback `~/.config/omarchy/current/theme/`), re-reads at most every 2 s, maps to semantic tokens; `HERMES_OMARCHY_THEME` overrides | `crates/hermes-desktop/src/theme.rs` |
| Single instance | Own IPC socket + `--toggle`; second launch forwards Show/Toggle | `ipc.rs`, `main.rs` |
| Tray | `ksni` tray with Show/Quit; window close hides to tray so turns keep streaming | `tray.rs` |
| Audio playback | `pw-play` → `paplay` → `aplay`, first on `PATH` | `voice.rs` |
| File dialogs | `rfd` with `xdg-portal` (works on Wayland/Hyprland) | pickers in `app.rs` |
| Clipboard | GPUI clipboard (Wayland data device) | copy reply, paste in editors |
| Dictation | External Superwhisper; the app never captures audio | README |

## Conventions to hook into (and what to change)

### 1. Launch and focus: use Omarchy's helper, not a custom socket

Omarchy ships `omarchy-launch-or-focus` (and `-or-focus-tui`,
`-or-focus-webapp`), which launches once and focuses the existing window after
that — the idiomatic Hyprland binding.

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
respects DND, stacks cards, supports action buttons (4.5+) and takes an argv
click command (4.0.1+). This is the correct channel for “turn finished”,
“approval needed” and routine results.

```bash
omarchy-notification-send \
  --app-name "Hermes" -u normal \
  "Turn finished" "ops · 3 tools · 12 s" \
  --exec hermes-desktop --toggle
```

Next step for the client: emit a notification when a turn completes while the
window is hidden, and route the click through `--toggle`. Clicking should also
work when the app is already focused (`omarchy-hyprland-focus-app` fallback).
Respect `Super + Comma` (act on newest card) by keeping one card per event type
and collapsing repeats.

### 3. Bar widget: an `omarchy-shell` plugin, not a tray icon

Omarchy 4.x has no separate Waybar system; a bar widget is a Quickshell plugin
with a `manifest.json` (`id`, `kinds: ["bar-widget"]`, `entryPoints.barWidget`,
optional `barWidget.defaultSection`) loaded by `omarchy-shell`. Placement is
managed with `omarchy bar move <id> --section left|center|right` and enabling
with `omarchy plugin enable <id>`.

Planned widget: connection state (green/amber/red), streaming indicator, pending
approval count; left click toggles the window, right click opens a panel with the
last reply. A widget cannot read the client's memory, so it needs a small
state channel — options, in preference order:

1. A Unix socket in the runtime dir that the widget queries (mirrors the IPC
   socket; no new daemon).
2. A `hermes status --json` CLI subcommand the widget shells out to.
3. A shell `service` plugin that subscribes to the same socket.

Ship it as a plugin users add with `omarchy plugin add <git-url>` (plugins land
disabled and are unsandboxed; document that warning).

### 4. Theme changes: prefer the `theme-set` hook over polling

Omarchy hooks install per event into `~/.config/omarchy/hooks/<event>.d/` with
`omarchy hook install theme-set <script>`. A hook can signal the running app
(e.g. `kill -USR1`, or a socket message) instead of polling.

Keep polling as the fallback for other desktops, but on Omarchy the hook is
exact: it fires after every `omarchy theme set`, including theme installs that
change fonts. Also read `shell.toml` (`font_family`, bar size) and the
`theme.name` file for display, not just `colors.toml`. Bracket the theme
directory when generating CSS/colour strings, as Omarchy's own templates do.

### 5. Screenshots and capture: reuse `omarchy capture`

`omarchy capture screenshot` and `omarchy screenrecord` are the desktop's capture
path (region/full screen, saving to the pictures directory). Instead of building
a capture pipeline, add “Attach screenshot” that invokes the same command and
feeds the resulting path into the existing attachment upload. This is the
cheapest path to visual bug reports and to attaching UI context to a prompt.

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

### 9. Diagnostics

`omarchy debug --no-sudo --print` gathers system state without prompting.
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
