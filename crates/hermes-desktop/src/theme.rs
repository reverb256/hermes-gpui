//! Design tokens for the Hermes window.
//!
//! The semantic names follow the GPUI Kit style guide. On Omarchy the palette
//! is loaded from the active theme's `colors.toml`; everywhere else (and when
//! the file is missing or malformed) the fallback palette below is used.
//!
//! The rules still hold:
//!
//! * **Tokens before values.** View code never writes a hex literal.
//! * **Semantic names, not paint names.** A theme fills roles, not widgets.
//! * **One accent.** `accent()` marks selection, focus and primary actions.
//! * **One spacing rhythm and reading measure.** Layout geometry stays below.
//!
//! `refresh()` re-reads the active theme at most every two seconds; the app's
//! timer calls it so a live `omarchy theme set` is reflected without a restart.
//! A `theme-set` hook can drop a marker file, which makes the next call reload
//! immediately instead of waiting for the poll.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

// ---- surfaces ----------------------------------------------------------
const DEFAULT_BG: u32 = 0x0b0d12;
const DEFAULT_BG_ELEV: u32 = 0x11141b;
const DEFAULT_SURFACE: u32 = 0x171b25;
const DEFAULT_SURFACE_HI: u32 = 0x1f2532;
const DEFAULT_BORDER: u32 = 0x262c3a;
const DEFAULT_BORDER_HI: u32 = 0x39415a;

// ---- ink ---------------------------------------------------------------
const DEFAULT_TEXT: u32 = 0xe6e9f2;
const DEFAULT_TEXT_MUTED: u32 = 0x9aa3b8;
const DEFAULT_TEXT_DIM: u32 = 0x6f788d;

// ---- meaning -----------------------------------------------------------
const DEFAULT_ACCENT: u32 = 0x7aa2f7;
const DEFAULT_ACCENT_HI: u32 = 0x93b4ff;
const DEFAULT_ACCENT_SOFT: u32 = 0x1b2740;
const DEFAULT_USER: u32 = 0x9ed072;
const DEFAULT_REASONING: u32 = 0xb48ef5;
const DEFAULT_REASONING_SOFT: u32 = 0x2a2340;
const DEFAULT_WARN: u32 = 0xe0af68;
const DEFAULT_WARN_SOFT: u32 = 0x2b2417;
const DEFAULT_DANGER: u32 = 0xf7768e;
const DEFAULT_BOT_ACCENT_CYAN: u32 = 0x7dcfff;
const DEFAULT_DANGER_SOFT: u32 = 0x2e1a20;

// ---- layout geometry ---------------------------------------------------
/// Fixed bot rail.
pub const RAIL_W: f32 = 248.;
/// Inspector drawer width.
pub const PANEL_W: f32 = 340.;
/// Header bar height.
pub const HEADER_H: f32 = 52.;
/// Footer status strip: gateway state plus model and version.
pub const STATUS_H: f32 = 26.;
/// Maximum width of the transcript, in pixels.
pub const READ_W: f32 = 720.;
/// Maximum width of a user bubble — narrower than the measure so turns read apart.
pub const BUBBLE_W: f32 = 560.;
/// Settings sheet (the vault): wide enough for key, description and actions.
pub const SHEET_W: f32 = 760.;
/// Settings sheet height, capped so it never becomes a full-screen page.
pub const SHEET_H: f32 = 560.;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Palette {
    pub bg: u32,
    pub bg_elev: u32,
    pub surface: u32,
    pub surface_hi: u32,
    pub border: u32,
    pub border_hi: u32,
    pub text: u32,
    pub text_muted: u32,
    pub text_dim: u32,
    pub accent: u32,
    pub accent_hi: u32,
    pub accent_soft: u32,
    pub user: u32,
    pub reasoning: u32,
    pub reasoning_soft: u32,
    pub warn: u32,
    pub warn_soft: u32,
    pub danger: u32,
    pub danger_soft: u32,
    pub bot_accent_cyan: u32,
    pub scrim: u32,
}

impl Palette {
    pub const DEFAULT: Self = Self {
        bg: DEFAULT_BG,
        bg_elev: DEFAULT_BG_ELEV,
        surface: DEFAULT_SURFACE,
        surface_hi: DEFAULT_SURFACE_HI,
        border: DEFAULT_BORDER,
        border_hi: DEFAULT_BORDER_HI,
        text: DEFAULT_TEXT,
        text_muted: DEFAULT_TEXT_MUTED,
        text_dim: DEFAULT_TEXT_DIM,
        accent: DEFAULT_ACCENT,
        accent_hi: DEFAULT_ACCENT_HI,
        accent_soft: DEFAULT_ACCENT_SOFT,
        user: DEFAULT_USER,
        reasoning: DEFAULT_REASONING,
        reasoning_soft: DEFAULT_REASONING_SOFT,
        warn: DEFAULT_WARN,
        warn_soft: DEFAULT_WARN_SOFT,
        danger: DEFAULT_DANGER,
        danger_soft: DEFAULT_DANGER_SOFT,
        bot_accent_cyan: DEFAULT_BOT_ACCENT_CYAN,
        scrim: 0xcc05070b,
    };
}

impl Default for Palette {
    fn default() -> Self {
        Self::DEFAULT
    }
}

static PALETTE: OnceLock<RwLock<Palette>> = OnceLock::new();
static LAST_REFRESH: Mutex<Option<Instant>> = Mutex::new(None);

fn state() -> &'static RwLock<Palette> {
    PALETTE.get_or_init(|| RwLock::new(load_palette()))
}

fn read() -> RwLockReadGuard<'static, Palette> {
    state()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn value(get: impl FnOnce(&Palette) -> u32) -> u32 {
    get(&read())
}

pub fn bg() -> u32 {
    value(|p| p.bg)
}
pub fn bg_elev() -> u32 {
    value(|p| p.bg_elev)
}
pub fn surface() -> u32 {
    value(|p| p.surface)
}
pub fn surface_hi() -> u32 {
    value(|p| p.surface_hi)
}
pub fn border() -> u32 {
    value(|p| p.border)
}
pub fn border_hi() -> u32 {
    value(|p| p.border_hi)
}
pub fn text() -> u32 {
    value(|p| p.text)
}
pub fn text_muted() -> u32 {
    value(|p| p.text_muted)
}
pub fn text_dim() -> u32 {
    value(|p| p.text_dim)
}
pub fn accent() -> u32 {
    value(|p| p.accent)
}
pub fn accent_hi() -> u32 {
    value(|p| p.accent_hi)
}
pub fn accent_soft() -> u32 {
    value(|p| p.accent_soft)
}
pub fn user() -> u32 {
    value(|p| p.user)
}
pub fn reasoning() -> u32 {
    value(|p| p.reasoning)
}
pub fn reasoning_soft() -> u32 {
    value(|p| p.reasoning_soft)
}
pub fn warn() -> u32 {
    value(|p| p.warn)
}
pub fn warn_soft() -> u32 {
    value(|p| p.warn_soft)
}
pub fn danger() -> u32 {
    value(|p| p.danger)
}
pub fn danger_soft() -> u32 {
    value(|p| p.danger_soft)
}
pub fn bot_accent_cyan() -> u32 {
    value(|p| p.bot_accent_cyan)
}
pub fn scrim() -> u32 {
    value(|p| p.scrim)
}

/// Re-read the active Omarchy theme, at most once every two seconds.
/// Returns true when the palette changed, so the caller can repaint.
/// The `theme-set` hook marker bypasses the throttle for one reload.
pub fn refresh() -> bool {
    let forced = std::fs::remove_file(crate::omarchy::theme_marker()).is_ok();
    {
        let mut last = LAST_REFRESH
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        if !forced && last.is_some_and(|at| now.duration_since(at) < Duration::from_secs(2)) {
            return false;
        }
        *last = Some(now);
    }
    let next = load_palette();
    let mut current = state()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if *current == next {
        false
    } else {
        *current = next;
        true
    }
}

fn load_palette() -> Palette {
    let Some(path) = theme_path() else {
        return Palette::DEFAULT;
    };
    match std::fs::read_to_string(path) {
        Ok(text) => palette_from_str(&text),
        Err(_) => Palette::DEFAULT,
    }
}

/// Omarchy's active theme palette. A missing file means the fallback.
fn theme_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(state.join("omarchy/current/theme/colors.toml"))
}

/// Where the palette is read from, for diagnostics.
pub fn theme_source() -> String {
    theme_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "none".into())
}

fn palette_from_str(text: &str) -> Palette {
    let mut colors: HashMap<String, u32> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('[') || line.starts_with('#') {
            continue;
        }
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        let raw = raw.trim().trim_matches('"');
        if let Some(hex) = parse_hex(raw) {
            colors.insert(key, hex);
        }
    }

    let get = |key: &str| colors.get(key).copied();
    let bg = get("background").unwrap_or(DEFAULT_BG);
    let text = get("foreground").unwrap_or(DEFAULT_TEXT);
    let lighter = get("lighter_background").unwrap_or_else(|| mix(bg, text, 0.08));
    let accent = get("accent")
        .or_else(|| get("blue"))
        .unwrap_or(DEFAULT_ACCENT);
    let user = get("green").unwrap_or(DEFAULT_USER);
    let reasoning = get("magenta").unwrap_or(DEFAULT_REASONING);
    let warn = get("yellow").unwrap_or(DEFAULT_WARN);
    let danger = get("red").unwrap_or(DEFAULT_DANGER);
    let cyan = get("cyan").unwrap_or(DEFAULT_BOT_ACCENT_CYAN);

    let bg_elev = mix(bg, lighter, 0.55);
    let surface_hi = mix(lighter, text, 0.08);
    let accent_soft = get("selection").unwrap_or_else(|| mix(bg, accent, 0.18));
    let planes = [bg, bg_elev, lighter, surface_hi, accent_soft];
    let muted = ensure_contrast(
        get("muted").unwrap_or_else(|| mix(text, bg, 0.35)),
        text,
        &planes,
        4.5,
    );
    let dim = ensure_contrast(
        get("dark_foreground").unwrap_or_else(|| mix(text, bg, 0.50)),
        text,
        &planes,
        3.0,
    );

    Palette {
        bg,
        bg_elev,
        surface: lighter,
        surface_hi,
        border: mix(bg, text, 0.18),
        border_hi: mix(bg, text, 0.30),
        text,
        text_muted: muted,
        text_dim: dim,
        accent,
        accent_hi: get("bright_blue").unwrap_or_else(|| mix(accent, text, 0.20)),
        accent_soft,
        user,
        reasoning,
        reasoning_soft: mix(bg, reasoning, 0.16),
        warn,
        warn_soft: mix(bg, warn, 0.16),
        danger,
        danger_soft: mix(bg, danger, 0.16),
        bot_accent_cyan: cyan,
        scrim: 0xcc00_0000 | (bg & 0x00ff_ffff),
    }
}

fn luminance(hex: u32) -> f32 {
    let channel = |shift: u32| {
        let raw = ((hex >> shift) & 0xff_u32) as f32 / 255.;
        if raw <= 0.04045 {
            raw / 12.92
        } else {
            ((raw + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

fn contrast(a: u32, b: u32) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Move low-contrast theme hints toward the theme foreground, without changing
/// the actual theme colors or assuming a dark palette.
fn ensure_contrast(color: u32, foreground: u32, planes: &[u32], minimum: f32) -> u32 {
    for step in 0..=100 {
        let candidate = mix(color, foreground, step as f32 / 100.);
        if planes
            .iter()
            .all(|plane| contrast(candidate, *plane) >= minimum)
        {
            return candidate;
        }
    }
    foreground
}

fn parse_hex(raw: &str) -> Option<u32> {
    let raw = raw.trim().trim_start_matches('#');
    if raw.len() != 6 {
        return None;
    }
    u32::from_str_radix(raw, 16).ok()
}

fn mix(a: u32, b: u32, amount: f32) -> u32 {
    let amount = amount.clamp(0.0, 1.0);
    let channel = |shift: u32| {
        let from = ((a >> shift) & 0xff) as f32;
        let to = ((b >> shift) & 0xff) as f32;
        (from + (to - from) * amount).round() as u32
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"
mode = "dark"
accent = "#7d82d9"
selection = "#252e56"
muted = "#6d7db6"
background = "#060B1E"
dark_background = "#040816"
lighter_background = "#131a3a"
foreground = "#ffcead"
dark_foreground = "#6d7db6"
red = "#ED5B5A"
yellow = "#E9BB4F"
green = "#92a593"
cyan = "#a3bfd1"
blue = "#7d82d9"
magenta = "#c89dc1"
bright_blue = "#c2c4f0"
"##;

    #[test]
    fn omarchy_colors_map_onto_the_semantic_palette() {
        let palette = palette_from_str(SAMPLE);
        assert_eq!(palette.bg, 0x060b1e);
        assert_eq!(palette.surface, 0x131a3a);
        assert_eq!(palette.text, 0xffcead);
        assert_eq!(palette.accent, 0x7d82d9);
        assert_eq!(palette.accent_soft, 0x252e56);
        assert_eq!(palette.user, 0x92a593);
        assert_eq!(palette.reasoning, 0xc89dc1);
        assert_eq!(palette.warn, 0xe9bb4f);
        assert_eq!(palette.danger, 0xed5b5a);
        assert_eq!(palette.bot_accent_cyan, 0xa3bfd1);
        assert_eq!(palette.scrim >> 24, 0xcc);
    }

    #[test]
    fn theme_hints_remain_readable_on_derived_planes() {
        let palette = palette_from_str(SAMPLE);
        for plane in [
            palette.bg,
            palette.bg_elev,
            palette.surface,
            palette.surface_hi,
            palette.accent_soft,
        ] {
            assert!(contrast(palette.text_muted, plane) >= 4.5);
            assert!(contrast(palette.text_dim, plane) >= 3.0);
        }
    }

    #[test]
    fn malformed_or_missing_values_fall_back() {
        let palette = palette_from_str("background = \"not-a-colour\"\n");
        assert_eq!(palette.bg, DEFAULT_BG);
        assert_eq!(palette.text, DEFAULT_TEXT);
        assert_eq!(palette.accent, DEFAULT_ACCENT);
    }

    #[test]
    fn derived_surfaces_move_away_from_the_background() {
        let palette = palette_from_str(SAMPLE);
        assert_ne!(palette.bg_elev, palette.bg);
        assert_ne!(palette.surface_hi, palette.surface);
        assert!(palette.border != palette.bg);
    }

    #[test]
    fn blending_is_bounded() {
        assert_eq!(mix(0x000000, 0xffffff, 0.0), 0x000000);
        assert_eq!(mix(0x000000, 0xffffff, 1.0), 0xffffff);
        assert_eq!(mix(0x000000, 0xffffff, 0.5), 0x808080);
    }

    #[test]
    fn body_copy_clears_aaa_on_every_default_plane() {
        let palette = Palette::DEFAULT;
        for plane in [
            palette.bg,
            palette.bg_elev,
            palette.surface,
            palette.surface_hi,
        ] {
            assert!(
                contrast(palette.text, plane) >= 7.0,
                "text on {plane:#08x} is {:.1}:1",
                contrast(palette.text, plane)
            );
        }
    }

    #[test]
    fn metadata_clears_aa_and_hints_stay_legible() {
        let palette = Palette::DEFAULT;
        for plane in [palette.bg, palette.bg_elev, palette.surface] {
            assert!(contrast(palette.text_muted, plane) >= 4.5);
            assert!(contrast(palette.text_dim, plane) >= 3.0);
        }
    }

    #[test]
    fn accent_and_status_colours_stay_readable() {
        let palette = Palette::DEFAULT;
        assert!(contrast(palette.accent, palette.bg) >= 4.5);
        assert!(contrast(palette.accent, palette.surface) >= 4.5);
        assert!(contrast(palette.user, palette.surface) >= 4.5);
        assert!(contrast(palette.warn, palette.surface) >= 4.5);
        assert!(contrast(palette.danger, palette.surface) >= 4.5);
        assert!(contrast(palette.reasoning, palette.surface) >= 4.5);
        assert!(contrast(palette.bot_accent_cyan, palette.surface) >= 4.5);
        assert!(contrast(palette.bg, palette.accent) >= 4.5);
    }

    #[test]
    fn layout_measures_stay_in_a_readable_band() {
        assert!((560. ..=760.).contains(&READ_W), "reading measure drifted");
        assert!((220. ..=300.).contains(&RAIL_W), "rail drifted");
        assert!((300. ..=420.).contains(&PANEL_W), "inspector drifted");
        const { assert!(BUBBLE_W <= READ_W) };
        const { assert!(HEADER_H >= 44., "header must stay a comfortable hit target") };
        assert!(
            (22. ..=32.).contains(&STATUS_H),
            "status strip must stay a strip"
        );
        assert!(
            (640. ..=900.).contains(&SHEET_W) && SHEET_H < 1080.,
            "settings sheet must stay a sheet, not a page"
        );
    }
}
