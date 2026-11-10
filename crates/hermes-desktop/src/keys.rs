//! Keyboard map, kept pure so it can be tested without a window.
//!
//! The goal is Grok-Bot-like flow: never reach for the mouse. Enter sends,
//! Shift+Enter breaks the line, Ctrl+K opens the palette, Ctrl+N starts a
//! fresh chat, Ctrl+1..9 jumps between bots (profiles), Ctrl+O attaches a
//! file, Esc interrupts or dismisses.

/// Which surface has the keystroke.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Composer,
    Palette,
    Mention,
}

/// One key press, normalised away from GPUI types.
#[derive(Clone, Copy, Debug)]
pub struct Chord<'a> {
    pub key: &'a str,
    /// Ctrl on Linux/Windows, Cmd on macOS (they are the same "command" key).
    pub primary: bool,
    pub shift: bool,
    pub alt: bool,
    /// The printable character this press produces, if any.
    pub text: Option<&'a str>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Send,
    Newline,
    Backspace,
    Paste,
    Insert(String),
    Interrupt,
    NewChat,
    OpenPalette,
    Refresh,
    Attach,
    ToggleInspector,
    ToggleVault,
    ToggleModels,
    ToggleAssignments,
    ToggleSkills,
    ToggleMcp,
    ToggleSessions,
    ToggleUsage,
    Find,
    Dismiss,
    PaletteUp,
    PaletteDown,
    PaletteAccept,
    ListUp,
    ListDown,
    ListAccept,
    SwitchProfile(usize),
    Nothing,
}

pub fn resolve(chord: Chord<'_>, focus: Focus) -> Action {
    match focus {
        Focus::Palette => match chord.key {
            "escape" => Action::Dismiss,
            "enter" => Action::PaletteAccept,
            "up" => Action::PaletteUp,
            "down" => Action::PaletteDown,
            "backspace" => Action::Backspace,
            _ => match chord.text {
                Some(text) if !chord.primary => Action::Insert(text.to_string()),
                _ => Action::Nothing,
            },
        },
        // The mention popup owns Enter/Tab/Esc; typing keeps filtering it.
        Focus::Mention => match chord.key {
            "escape" => Action::Dismiss,
            "enter" | "tab" => Action::ListAccept,
            "up" => Action::ListUp,
            "down" => Action::ListDown,
            "backspace" => Action::Backspace,
            _ => match chord.text {
                Some(text) if !chord.primary => Action::Insert(text.to_string()),
                _ => Action::Nothing,
            },
        },
        Focus::Composer => {
            if chord.primary {
                return match chord.key {
                    "x" if chord.shift => Action::ToggleMcp,
                    "k" if chord.shift => Action::ToggleSkills,
                    "k" => Action::OpenPalette,
                    "n" => Action::NewChat,
                    "r" => Action::Refresh,
                    "f" => Action::Find,
                    "o" => Action::Attach,
                    "," | "comma" => Action::ToggleInspector,
                    "v" if chord.shift => Action::ToggleVault,
                    "v" => Action::Paste,
                    "m" if chord.shift => Action::ToggleModels,
                    "a" if chord.shift => Action::ToggleAssignments,
                    "s" if chord.shift => Action::ToggleSessions,
                    "u" if chord.shift => Action::ToggleUsage,
                    digit => match digit.parse::<usize>() {
                        Ok(index) if (1..=9).contains(&index) => Action::SwitchProfile(index),
                        _ => Action::Nothing,
                    },
                };
            }

            match chord.key {
                "enter" if chord.shift => Action::Newline,
                "enter" => Action::Send,
                "escape" => Action::Interrupt,
                "backspace" => Action::Backspace,
                _ => match chord.text {
                    Some(text) if !chord.alt => Action::Insert(text.to_string()),
                    _ => Action::Nothing,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord<'a>(key: &'a str, text: Option<&'a str>) -> Chord<'a> {
        Chord {
            key,
            primary: false,
            shift: false,
            alt: false,
            text,
        }
    }

    #[test]
    fn enter_sends_and_shift_enter_breaks_the_line() {
        assert_eq!(resolve(chord("enter", None), Focus::Composer), Action::Send);
        assert_eq!(
            resolve(
                Chord {
                    shift: true,
                    ..chord("enter", None)
                },
                Focus::Composer
            ),
            Action::Newline
        );
    }

    #[test]
    fn ctrl_shortcuts_are_command_shortcuts() {
        let ctrl = |key| Chord {
            primary: true,
            ..chord(key, None)
        };
        assert_eq!(resolve(ctrl("k"), Focus::Composer), Action::OpenPalette);
        assert_eq!(resolve(ctrl("n"), Focus::Composer), Action::NewChat);
        assert_eq!(resolve(ctrl("r"), Focus::Composer), Action::Refresh);
        assert_eq!(resolve(ctrl("v"), Focus::Composer), Action::Paste);
        assert_eq!(resolve(ctrl("o"), Focus::Composer), Action::Attach);
        assert_eq!(resolve(ctrl("f"), Focus::Composer), Action::Find);
        assert_eq!(resolve(ctrl(","), Focus::Composer), Action::ToggleInspector);
        assert_eq!(
            resolve(
                Chord {
                    primary: true,
                    shift: true,
                    ..chord("v", None)
                },
                Focus::Composer
            ),
            Action::ToggleVault
        );
        assert_eq!(
            resolve(
                Chord {
                    primary: true,
                    shift: true,
                    ..chord("u", None)
                },
                Focus::Composer
            ),
            Action::ToggleUsage
        );
        assert_eq!(
            resolve(ctrl("3"), Focus::Composer),
            Action::SwitchProfile(3)
        );
        assert_eq!(resolve(ctrl("0"), Focus::Composer), Action::Nothing);
    }

    #[test]
    fn printable_keys_insert_text() {
        assert_eq!(
            resolve(chord("a", Some("a")), Focus::Composer),
            Action::Insert("a".into())
        );
        assert_eq!(
            resolve(chord("space", Some(" ")), Focus::Composer),
            Action::Insert(" ".into())
        );
    }

    #[test]
    fn escape_interrupts_the_turn() {
        assert_eq!(
            resolve(chord("escape", None), Focus::Composer),
            Action::Interrupt
        );
    }

    #[test]
    fn palette_swallows_typing_into_its_query() {
        assert_eq!(
            resolve(chord("a", Some("a")), Focus::Palette),
            Action::Insert("a".into())
        );
        assert_eq!(
            resolve(chord("enter", None), Focus::Palette),
            Action::PaletteAccept
        );
        assert_eq!(
            resolve(chord("down", None), Focus::Palette),
            Action::PaletteDown
        );
        assert_eq!(
            resolve(chord("escape", None), Focus::Palette),
            Action::Dismiss
        );
        assert_eq!(
            resolve(
                Chord {
                    primary: true,
                    ..chord("k", Some("k"))
                },
                Focus::Palette
            ),
            Action::Nothing
        );
    }

    #[test]
    fn mentions_accept_on_enter_or_tab() {
        assert_eq!(
            resolve(chord("enter", None), Focus::Mention),
            Action::ListAccept
        );
        assert_eq!(
            resolve(chord("tab", None), Focus::Mention),
            Action::ListAccept
        );
        assert_eq!(resolve(chord("up", None), Focus::Mention), Action::ListUp);
        assert_eq!(
            resolve(chord("escape", None), Focus::Mention),
            Action::Dismiss
        );
        assert_eq!(
            resolve(chord("m", Some("m")), Focus::Mention),
            Action::Insert("m".into())
        );
    }
}
