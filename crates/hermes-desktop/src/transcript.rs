//! Transcript model. Kept free of GPUI so the streaming merge rules can be
//! tested without a window.

/// Who produced a line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    User,
    Assistant,
    Reasoning,
    System,
}

/// One rendered block.
#[derive(Clone, Debug)]
pub struct Line {
    pub role: Role,
    pub text: String,
}

impl Line {
    pub fn new(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
        }
    }
}

/// Append streamed text to the trailing line of the same role, or start a new
/// one when the speaker changed. Deltas from the gateway arrive per token, so
/// this runs once per frame.
pub fn append_streaming(lines: &mut Vec<Line>, role: Role, text: &str) {
    match lines.last_mut() {
        Some(line) if line.role == role => line.text.push_str(text),
        _ => lines.push(Line::new(role, text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consecutive_deltas_merge_into_one_line() {
        let mut lines = vec![Line::new(Role::User, "hi")];
        append_streaming(&mut lines, Role::Assistant, "Hel");
        append_streaming(&mut lines, Role::Assistant, "lo");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].text, "Hello");
    }

    #[test]
    fn role_change_starts_a_new_line() {
        let mut lines = vec![Line::new(Role::User, "hi")];
        append_streaming(&mut lines, Role::Assistant, "answer");
        append_streaming(&mut lines, Role::Reasoning, "because");
        append_streaming(&mut lines, Role::Assistant, "more");
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[2].role, Role::Reasoning);
        assert_eq!(lines[3].text, "more");
    }

    #[test]
    fn streaming_into_empty_transcript_creates_a_line() {
        let mut lines = Vec::new();
        append_streaming(&mut lines, Role::System, "connected");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "connected");
    }
}
