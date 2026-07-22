//! Compact human-view contract for CLI and MCP presenters.

use super::view::{CanonicalHumanView, HumanFieldValue};

/// The reviewed revision of the compact human presentation contract.
pub const HUMAN_VIEW_REVISION: u32 = 2;

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

pub(crate) fn render(view: CanonicalHumanView) -> MarkdownView {
    let mut text = format!("## {}\n", escape_text(&view.heading));
    for field in view.fields {
        text.push_str("\n- ");
        text.push_str(field.label);
        text.push_str(": ");
        match field.value {
            HumanFieldValue::Code(value) => {
                text.push('`');
                text.push_str(&value.replace('`', "\\`"));
                text.push('`');
            }
            HumanFieldValue::Text(value) => text.push_str(&escape_text(&value)),
        }
    }
    MarkdownView::new(text)
}

fn escape_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', " ")
        .replace('\r', " ")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('`', "\\`")
}

#[cfg(test)]
mod tests {
    use super::{HUMAN_VIEW_REVISION, MarkdownView, render};
    use crate::cli::output::view::{CanonicalHumanView, HumanField, HumanFieldValue};

    #[test]
    fn carries_the_current_human_view_revision() {
        let view = MarkdownView::new("result");

        assert_eq!(view.revision(), HUMAN_VIEW_REVISION);
        assert_eq!(view.as_str(), "result");
    }

    #[test]
    fn canonical_markdown_golden_formats_only_the_supplied_view() {
        let rendered = render(CanonicalHumanView {
            heading: "feedback_list".to_owned(),
            fields: vec![
                HumanField {
                    label: "Status",
                    value: HumanFieldValue::Code("problem".to_owned()),
                },
                HumanField {
                    label: "Message",
                    value: HumanFieldValue::Text("Retry *after* refresh".to_owned()),
                },
            ],
        });

        assert_eq!(
            rendered.as_str(),
            "## feedback\\_list\n\n- Status: `problem`\n- Message: Retry \\*after\\* refresh"
        );
    }
}
