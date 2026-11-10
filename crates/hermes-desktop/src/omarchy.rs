//! Omarchy desktop conventions used by the client.
//!
//! All of this is optional: every function degrades to "not available" off
//! Omarchy, so the same binary keeps working on a plain Hyprland or X11 desktop.
//!
//! * Notifications go through `omarchy-notification-send` (the shell owns the
//!   notification server) with `--replace-id` so repeats collapse into one card.
//! * Screenshots go through `omarchy capture screenshot`, saved into a cache
//!   directory we control so the new file can be found deterministically.
//! * OCR goes through `omarchy capture text`, which copies the result to the
//!   clipboard; we read it back with `wl-paste`.
//! * `theme-set` is a hook directory (`~/.config/omarchy/hooks/theme-set.d/`);
//!   our hook just touches a marker that `theme::refresh` checks, so a theme
//!   switch repaints immediately instead of waiting for the next poll.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::SystemTime;

use crate::status;

const APP_NAME: &str = "Hermes";

static TURN_CARD: Mutex<Option<String>> = Mutex::new(None);
static APPROVAL_CARD: Mutex<Option<String>> = Mutex::new(None);

/// Is `binary` resolvable on PATH?
pub fn has(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|dir| {
                let candidate = dir.join(binary);
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

/// True when this session can talk to the Omarchy notification server.
pub fn notifications_available() -> bool {
    has("omarchy-notification-send")
}

/// Notify that a turn finished, collapsing repeats into one card.
/// The click toggles the window; the action schedules an Omarchy reminder.
pub fn notify_turn(summary: &str) -> bool {
    let body = clip(summary, 140);
    let mut args = base_args(&TURN_CARD, "normal");
    args.push("Turn finished".into());
    args.push(body.clone());
    args.push("--exec".into());
    args.push("hermes-desktop".into());
    args.push("--toggle".into());
    if has("omarchy") {
        args.push("--action".into());
        args.push("Remind in 15".into());
        args.push("omarchy".into());
        args.push("reminder".into());
        args.push("15".into());
        args.push(format!("Hermes: {body}"));
    }
    send(&args, &TURN_CARD)
}

/// Notify that a tool call needs an answer. Critical urgency so it surfaces
/// even when the user is working elsewhere.
pub fn notify_approval(summary: &str) -> bool {
    let body = clip(summary, 140);
    let mut args = base_args(&APPROVAL_CARD, "critical");
    args.push("Approval needed".into());
    args.push(body.clone());
    args.push("--exec".into());
    args.push("hermes-desktop".into());
    args.push("--toggle".into());
    args.push("--action".into());
    args.push("Open".into());
    args.push("hermes-desktop".into());
    args.push("--toggle".into());
    if has("omarchy") {
        args.push("--action".into());
        args.push("Remind in 15".into());
        args.push("omarchy".into());
        args.push("reminder".into());
        args.push("15".into());
        args.push(format!("Hermes needs approval: {body}"));
    }
    send(&args, &APPROVAL_CARD)
}

fn base_args(card: &Mutex<Option<String>>, urgency: &str) -> Vec<String> {
    let mut args = vec![
        "--app-name".to_string(),
        APP_NAME.to_string(),
        "-u".to_string(),
        urgency.to_string(),
        "-p".to_string(),
    ];
    if let Ok(guard) = card.lock()
        && let Some(id) = guard.as_ref()
    {
        args.push("-r".to_string());
        args.push(id.clone());
    }
    args
}

/// Send the notification and remember the id so the next one replaces it.
fn send(args: &[String], card: &Mutex<Option<String>>) -> bool {
    if !notifications_available() {
        return false;
    }
    let output = Command::new("omarchy-notification-send")
        .args(args)
        .output();
    let Ok(output) = output else {
        tracing::warn!("omarchy-notification-send could not be spawned");
        return false;
    };
    if !output.status.success() {
        tracing::warn!(
            status = %output.status,
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "omarchy-notification-send failed"
        );
        return false;
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !id.is_empty()
        && id.chars().all(|ch| ch.is_ascii_digit())
        && let Ok(mut guard) = card.lock()
    {
        *guard = Some(id);
    }
    true
}

/// Screenshots we take go here so the fresh file can be identified.
fn shot_dir() -> PathBuf {
    status::cache_dir().join("screenshots")
}

/// Capture a region through `omarchy capture screenshot` and return the file.
pub async fn screenshot() -> Result<PathBuf, String> {
    if !has("omarchy") {
        return Err("omarchy is not on PATH".into());
    }
    let dir = shot_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let before = newest_image(&dir);
    let status = tokio::process::Command::new("omarchy")
        .args(["capture", "screenshot", "region", "save"])
        .env("OMARCHY_SCREENSHOT_DIR", &dir)
        .status()
        .await
        .map_err(|err| format!("omarchy capture screenshot: {err}"))?;
    if !status.success() {
        return Err(format!("omarchy capture screenshot exited with {status}"));
    }
    let found = newest_image(&dir);
    match found {
        Some(path) if Some(&path) != before.as_ref() => Ok(path),
        _ => Err("no screenshot was saved (cancelled?)".into()),
    }
}

/// Newest image in `dir`, if any.
fn newest_image(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase);
        if !matches!(extension.as_deref(), Some("png" | "jpg" | "jpeg" | "webp")) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(at, _)| modified > *at) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

/// OCR a region through `omarchy capture text`, which copies the result to the
/// clipboard; read it back so the composer can show it for review.
pub async fn capture_text() -> Result<String, String> {
    if !has("omarchy") {
        return Err("omarchy is not on PATH".into());
    }
    let status = tokio::process::Command::new("omarchy")
        .args(["capture", "text"])
        .status()
        .await
        .map_err(|err| format!("omarchy capture text: {err}"))?;
    if !status.success() {
        return Err(format!("omarchy capture text exited with {status}"));
    }
    let output = tokio::process::Command::new("wl-paste")
        .args(["--no-newline"])
        .output()
        .await
        .map_err(|err| format!("wl-paste: {err}"))?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        Err("no text was found in the selection".into())
    } else {
        Ok(text)
    }
}

/// Where the `theme-set` hook drops its marker.
pub fn theme_marker() -> PathBuf {
    status::cache_dir().join("theme-dirty")
}

/// Install a `theme-set` hook that forces the next theme poll to reload.
/// Returns the installed script path.
pub fn install_hooks() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())?;
    let dir = home.join(".config/omarchy/hooks/theme-set.d");
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let path = dir.join("hermes-gpui-theme");
    let marker = theme_marker();
    let script = format!(
        "#!/bin/sh\n\
         # Installed by hermes-desktop --install-omarchy-hooks.\n\
         # Marks the palette dirty so the running client repaints without polling.\n\
         mkdir -p '{parent}'\n\
         touch '{marker}'\n",
        parent = marker
            .parent()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| "/tmp".into()),
        marker = marker.display(),
    );
    std::fs::write(&path, script)
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|err| format!("cannot chmod {}: {err}", path.display()))?;
    }
    Ok(path)
}

/// A block of local facts for support requests (`--diagnostics`).
pub fn diagnostics() -> String {
    let mut lines = Vec::new();
    let version = Command::new("omarchy")
        .arg("version")
        .output()
        .ok()
        .map(|output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            format!("{}{}", stdout.trim(), stderr.trim())
                .trim()
                .to_string()
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "not detected".into());
    lines.push(format!("omarchy: {version}"));
    lines.push(format!(
        "theme: {}",
        crate::theme::active_theme_name().unwrap_or_else(|| "none".into())
    ));
    let metrics = crate::theme::shell_metrics();
    lines.push(format!(
        "shell: bar {} px, font {} px, scale-with-font {}",
        metrics.bar_height, metrics.font_size, metrics.scale_with_font
    ));
    for binary in [
        "omarchy-notification-send",
        "wl-paste",
        "omasnap",
        "grim",
        "tesseract",
        "hermes-desktop",
    ] {
        lines.push(format!(
            "{binary}: {}",
            if has(binary) { "yes" } else { "no" }
        ));
    }
    lines.push(format!("cache: {}", status::cache_dir().display()));
    lines.push(format!("status: {}", status::status_path().display()));
    lines.push(format!(
        "host: {}",
        std::env::var("HERMES_URL").unwrap_or_else(|_| "default".into())
    ));
    lines.push(format!(
        "socket: {}",
        std::env::var("HERMES_GPUI_SOCKET").unwrap_or_else(|_| "default".into())
    ));
    lines.join("\n")
}

fn clip(text: &str, max: usize) -> String {
    let text = text.trim().replace('\n', " ");
    if text.chars().count() <= max {
        return text;
    }
    let shortened: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", shortened.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_keeps_words_and_respects_the_limit() {
        assert_eq!(clip("short", 20), "short");
        let clipped = clip("a long line\nwith a break", 10);
        assert!(clipped.chars().count() <= 10);
        assert!(!clipped.contains('\n'));
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn missing_binaries_are_reported_not_panicked() {
        // Not a hard guarantee: only that lookup is total.
        let _ = has("definitely-not-a-real-binary-xyz");
    }
}
