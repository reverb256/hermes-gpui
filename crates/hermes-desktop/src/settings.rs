//! The settings window: one sheet with the sections down its left edge.
//!
//! Only sections with a working wire surface live here. Vault and Usage &
//! Billing are those sections; a nav entry that opened an empty page would be a
//! stub, so none is listed.

/// A page in the settings sheet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Vault,
    Model,
    Assignments,
    Skills,
    Mcp,
    Usage,
}

impl Section {
    pub fn all() -> [Section; 6] {
        [
            Section::Vault,
            Section::Model,
            Section::Assignments,
            Section::Skills,
            Section::Mcp,
            Section::Usage,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Section::Vault => "Vault",
            Section::Model => "Model",
            Section::Assignments => "Assignments",
            Section::Skills => "Skills",
            Section::Mcp => "MCP",
            Section::Usage => "Usage & Billing",
        }
    }

    /// Monochrome glyph; the row's ink colour decides contrast.
    pub fn glyph(self) -> &'static str {
        match self {
            Section::Vault => "▤",
            Section::Model => "◇",
            Section::Assignments => "▦",
            Section::Skills => "✦",
            Section::Mcp => "⇄",
            Section::Usage => "▥",
        }
    }
}

pub struct Settings {
    pub open: bool,
    pub section: Section,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            open: false,
            section: Section::Vault,
        }
    }
}

impl Settings {
    pub fn show(&mut self, section: Section) {
        self.open = true;
        self.section = section;
    }

    pub fn hide(&mut self) {
        self.open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_are_distinct_and_labeled() {
        let sections = Section::all();
        assert_eq!(sections.len(), 6);
        for (index, section) in sections.iter().enumerate() {
            assert!(!section.label().is_empty());
            assert!(
                sections[..index]
                    .iter()
                    .all(|other| other.label() != section.label())
            );
        }
    }

    #[test]
    fn showing_a_section_opens_the_sheet_on_it() {
        let mut settings = Settings::default();
        assert!(!settings.open);
        settings.show(Section::Usage);
        assert!(settings.open);
        assert_eq!(settings.section, Section::Usage);
        settings.hide();
        assert!(!settings.open);
        assert_eq!(
            settings.section,
            Section::Usage,
            "closing keeps the section"
        );
    }
}
