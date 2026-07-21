//! Compact human-view contract for CLI and MCP presenters.

/// The reviewed revision of the compact human presentation contract.
pub const HUMAN_VIEW_REVISION: u32 = 1;

/// Presentation-ready human output with its contract revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownView {
    revision: u32,
    text: String,
}

impl MarkdownView {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            revision: HUMAN_VIEW_REVISION,
            text: text.into(),
        }
    }

    pub const fn revision(&self) -> u32 {
        self.revision
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::{HUMAN_VIEW_REVISION, MarkdownView};

    #[test]
    fn carries_the_current_human_view_revision() {
        let view = MarkdownView::new("result");

        assert_eq!(view.revision(), HUMAN_VIEW_REVISION);
        assert_eq!(view.as_str(), "result");
    }
}
