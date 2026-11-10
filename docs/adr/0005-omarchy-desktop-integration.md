# 0005. Integrate with Omarchy through its documented surfaces

- **Status:** Accepted
- **Date:** 2026-11-10
- **Deciders:** client maintainers

## Context

Omarchy 4.x is Arch + Hyprland + a single Quickshell process (`omarchy-shell`):
the bar, panels, overlays, menus and notification server are plugins inside it,
Hyprland config is Lua under `~/.config/hypr/`, themes are generated from
`colors.toml` into `~/.local/state/omarchy/current/theme/`, and Superwhisper is
the dictation default. A desktop app can either integrate through these surfaces
or fight them with its own tray, notifier and theme reader.

## Decision

Integrate only through Omarchy's documented, user-facing surfaces:

- **Theme**: read the generated palette (with `HERMES_OMARCHY_THEME` override);
  add a `theme-set` hook so updates are event-driven; keep polling as fallback
  for other desktops.
- **Notifications**: use `omarchy-notification-send` (never raw `notify-send`),
  with `--app-name`, urgency, argv `--exec` and click routing to focus/toggle.
- **Bar**: ship an `omarchy-shell` plugin (`bar-widget` kind) rather than a
  tray-only presence; state comes from the client's IPC socket or a
  `status --json` subcommand, not from importing app internals.
- **Launch**: document `omarchy-launch-or-focus` in `bindings.lua`; keep
  `--toggle` for other desktops.
- **Capture**: reuse `omarchy capture` for screenshot attachment instead of
  building a capture pipeline.
- **Dictation**: stay Superwhisper-first; never capture audio in-app, and treat
  dictated text as pasted input at the cursor.
- Never edit `/usr/share/omarchy/`; user config stays in `~/.config/`.

## Consequences

- The app behaves like a first-class Omarchy citizen and follows theme, DND,
  idle and notification conventions for free.
- Omarchy-specific code paths are additive; other desktops keep the polling and
  tray fallbacks.
- Omarchy 4.x is young (this host reports 4.0.0.alpha, upstream 4.5); commands
  must be discovered (`omarchy commands --json`) and missing surfaces handled
  gracefully.
- The bar plugin is unsandboxed Quickshell code, so it must be small, auditable
  and shipped opt-in with the standard warning.
