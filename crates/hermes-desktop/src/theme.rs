//! Design tokens for the Hermes window.
//!
//! The rules follow the GPUI Kit style guide (<https://gpui-kit.com/docs/style>),
//! which is the published style guidance for GPUI applications:
//!
//! * **Tokens before values.** Every colour, measure and radius lives here; the
//!   view code never writes a hex literal. Changing the theme means editing this
//!   file only.
//! * **Semantic names, not paint names.** `SURFACE` / `BORDER` / `TEXT_MUTED`
//!   describe the role, so a light theme can fill the same names later.
//! * **One accent.** `ACCENT` marks selection, focus and primary actions;
//!   `USER`, `REASONING`, `WARN` and `DANGER` are reserved for meaning.
//! * **One spacing rhythm.** Siblings are separated with the 4 px `gap_1..gap_6`
//!   helpers and surfaces pad with `p_2..p_5`; only layout geometry (rail width,
//!   header height, reading measure) is named here.
//! * **One reading measure.** Body text never exceeds `READ_W`, which keeps line
//!   length in a comfortable 60–75 character band.
//!
//! Contrast invariants are unit-tested at the bottom of this file.

// ---- surfaces ----------------------------------------------------------
/// Window plane: the transcript canvas.
pub const BG: u32 = 0x0b0d12;
/// Chrome plane: header and sidebar, one step above the window.
pub const BG_ELEV: u32 = 0x11141b;
/// Raised plane: cards, composer, popovers.
pub const SURFACE: u32 = 0x171b25;
/// Raised plane under the pointer or selection.
pub const SURFACE_HI: u32 = 0x1f2532;
/// Hairline between planes.
pub const BORDER: u32 = 0x262c3a;
/// Hairline for focused or emphasised containers.
pub const BORDER_HI: u32 = 0x39415a;

// ---- ink ---------------------------------------------------------------
/// Primary copy.
pub const TEXT: u32 = 0xe6e9f2;
/// Secondary copy: metadata, hints, labels.
pub const TEXT_MUTED: u32 = 0x9aa3b8;
/// Tertiary copy: keyboard hints and disabled glyphs.
pub const TEXT_DIM: u32 = 0x6f788d;

// ---- meaning -----------------------------------------------------------
/// Selection, focus, primary buttons.
pub const ACCENT: u32 = 0x7aa2f7;
/// Accent under the pointer.
pub const ACCENT_HI: u32 = 0x93b4ff;
/// Accent-tinted fill for chips and selected rows.
pub const ACCENT_SOFT: u32 = 0x1b2740;
/// The human in the conversation.
pub const USER: u32 = 0x9ed072;
/// Model reasoning, kept quieter than the answer.
pub const REASONING: u32 = 0xb48ef5;
/// Reasoning rule.
pub const REASONING_SOFT: u32 = 0x2a2340;
/// Waiting on a human decision.
pub const WARN: u32 = 0xe0af68;
/// Warning chip fill.
pub const WARN_SOFT: u32 = 0x2b2417;
/// Destructive actions and failures.
pub const DANGER: u32 = 0xf7768e;
/// A sixth bot accent, for rosters that need more colours than meanings.
pub const BOT_ACCENT_CYAN: u32 = 0x7dcfff;
/// Failure chip fill.
pub const DANGER_SOFT: u32 = 0x2e1a20;

// ---- layout geometry ---------------------------------------------------
/// Fixed bot rail.
pub const RAIL_W: f32 = 248.;
/// Inspector drawer width.
pub const PANEL_W: f32 = 340.;
/// Header bar height.
pub const HEADER_H: f32 = 52.;
/// Maximum width of the transcript, in pixels.
pub const READ_W: f32 = 720.;
/// Maximum width of a user bubble — narrower than the measure so turns read apart.
pub const BUBBLE_W: f32 = 560.;
/// Settings sheet (the vault): wide enough for key, description and actions.
pub const SHEET_W: f32 = 760.;
/// Settings sheet height, capped so it never becomes a full-screen page.
pub const SHEET_H: f32 = 560.;

// ---- scrim -------------------------------------------------------------
/// ARGB dimming plane between the window and a modal sheet (alpha in the top
/// byte, so `rgba(SCRIM)` scrims what is behind it).
pub const SCRIM: u32 = 0xcc05070b;

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(value: u32, shift: u32) -> f32 {
        let raw = ((value >> shift) & 0xff) as f32 / 255.;
        if raw <= 0.03928 {
            raw / 12.92
        } else {
            ((raw + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(hex: u32) -> f32 {
        0.2126 * channel(hex, 16) + 0.7152 * channel(hex, 8) + 0.0722 * channel(hex, 0)
    }

    fn contrast(a: u32, b: u32) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn body_copy_clears_aaa_on_every_plane() {
        for plane in [BG, BG_ELEV, SURFACE, SURFACE_HI] {
            assert!(
                contrast(TEXT, plane) >= 7.0,
                "TEXT on {plane:#08x} is {:.1}:1",
                contrast(TEXT, plane)
            );
        }
    }

    #[test]
    fn metadata_clears_aa_and_hints_stay_legible() {
        for plane in [BG, BG_ELEV, SURFACE] {
            assert!(contrast(TEXT_MUTED, plane) >= 4.5);
            assert!(contrast(TEXT_DIM, plane) >= 3.0);
        }
    }

    #[test]
    fn accent_and_status_colours_stay_readable() {
        assert!(contrast(ACCENT, BG) >= 4.5);
        assert!(contrast(ACCENT, SURFACE) >= 4.5);
        assert!(contrast(USER, SURFACE) >= 4.5);
        assert!(contrast(WARN, SURFACE) >= 4.5);
        assert!(contrast(DANGER, SURFACE) >= 4.5);
        assert!(contrast(REASONING, SURFACE) >= 4.5);
        assert!(contrast(BOT_ACCENT_CYAN, SURFACE) >= 4.5);
        // Primary buttons reverse the contrast: dark ink on the accent fill.
        assert!(contrast(BG, ACCENT) >= 4.5);
    }

    #[test]
    fn layout_measures_stay_in_a_readable_band() {
        assert!((560. ..=760.).contains(&READ_W), "reading measure drifted");
        assert!((220. ..=300.).contains(&RAIL_W), "rail drifted");
        assert!((300. ..=420.).contains(&PANEL_W), "inspector drifted");
        assert!(BUBBLE_W <= READ_W);
        assert!(HEADER_H >= 44., "header must stay a comfortable hit target");
        assert!(
            (640. ..=900.).contains(&SHEET_W) && SHEET_H < 1080.,
            "settings sheet must stay a sheet, not a page"
        );
    }
}
