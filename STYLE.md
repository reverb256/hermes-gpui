# Style guide

The UI follows the [GPUI Kit style guide](https://gpui-kit.com/docs/style) — the
published styling guidance for GPUI applications — specialised for this app.
Everything concrete lives in `crates/hermes-desktop/src/theme.rs`.

## Rules

1. **Tokens before values.** No hex literal, magic width or ad-hoc radius outside
   `theme.rs`. If a value is not a token, use the named scale helper
   (`gap_2`, `p_3`, `rounded_lg`) instead of inventing a number.
2. **Semantic names.** `BG` / `BG_ELEV` / `SURFACE` / `SURFACE_HI` are planes;
   `BORDER` / `BORDER_HI` are hairlines; `TEXT` / `TEXT_MUTED` / `TEXT_DIM` are
   ink levels. Views never talk about "dark grey".
3. **One accent.** `ACCENT` means selection, focus or the primary action. `USER`,
   `REASONING`, `WARN`, `DANGER` carry meaning and are never decorative.
4. **One rhythm.** Siblings get `gap_*` from the 4 px scale, surfaces get
   `p_*`/`px_*`/`py_*`. Vertical rhythm inside a card is `gap_3` (12 px); between
   transcript turns it is `gap_5` (24 px).
5. **One measure.** Transcript copy is capped at `READ_W` (720 px, ~70 characters)
   and user bubbles at `BUBBLE_W`.
6. **Geometry states attachment.** The window and sidebar are flat planes;
   cards, popovers and the composer are raised `SURFACE` with a `rounded_xl`
   radius and a hairline border. No drop shadows: elevation is colour + border.
7. **Hover, focus, selection.** Hover shifts one plane (`SURFACE` →
   `SURFACE_HI`); focus draws `ACCENT` on the composer border; selection uses
   `ACCENT_SOFT` fill plus `ACCENT` ink. Every interactive div needs `.id(...)`.
8. **Quiet labels.** Metadata is `text_xs` + `TEXT_MUTED`; eyebrows are
   `text_xs` + `BOLD` + `TEXT_DIM`. Body copy is `text_sm` + `TEXT`.

## Tokens

| Token | Value | Role |
| --- | --- | --- |
| `BG` | `#0b0d12` | window plane |
| `BG_ELEV` | `#11141b` | header, sidebar |
| `SURFACE` | `#171b25` | cards, composer, popovers |
| `SURFACE_HI` | `#1f2532` | hover, selected row |
| `BORDER` | `#262c3a` | hairline |
| `BORDER_HI` | `#39415a` | emphasised hairline |
| `TEXT` | `#e6e9f2` | body copy |
| `TEXT_MUTED` | `#9aa3b8` | metadata |
| `TEXT_DIM` | `#6f788d` | key hints |
| `ACCENT` | `#7aa2f7` | selection, focus, primary |
| `ACCENT_HI` | `#93b4ff` | accent hover |
| `ACCENT_SOFT` | `#1b2740` | accent chip fill |
| `USER` | `#9ed072` | the human |
| `REASONING` | `#b48ef5` | reasoning trace |
| `WARN` | `#e0af68` | awaiting a decision |
| `DANGER` | `#f7768e` | failure, deny |

## Verification

`cargo test -p hermes-desktop theme::tests` checks WCAG contrast for body copy
(AAA on every plane), metadata (AA), accent and status colours, plus the layout
band for the reading measure and rail.
