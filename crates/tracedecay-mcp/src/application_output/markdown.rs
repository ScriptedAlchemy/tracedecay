//! Compact human-view contract for CLI and MCP presenters.

use super::view::{CanonicalHumanView, HumanFieldValue};

/// Presentation-ready human output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownView {
    text: String,
}

impl MarkdownView {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

#[hotpath::measure(label = "mcp.application_output.render")]
pub fn render(view: CanonicalHumanView) -> MarkdownView {
    let mut text = format!("## {}\n", escape_text(&view.heading));
    for field in view.fields {
        match field.value {
            HumanFieldValue::Block(value) => {
                text.push_str("\n### ");
                text.push_str(field.label);
                text.push_str("\n\n");
                for line in value.lines() {
                    text.push_str("    ");
                    text.push_str(line);
                    text.push('\n');
                }
                if value.is_empty() {
                    text.push_str("    \n");
                }
            }
            HumanFieldValue::Code(value) => {
                text.push_str("\n- ");
                text.push_str(field.label);
                text.push_str(": ");
                text.push('`');
                text.push_str(&value.replace('`', "\\`"));
                text.push('`');
            }
            HumanFieldValue::Text(value) => {
                text.push_str("\n- ");
                text.push_str(field.label);
                text.push_str(": ");
                text.push_str(&escape_text(&value));
            }
        }
    }
    MarkdownView::new(text)
}

fn escape_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(['\n', '\r'], " ")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('`', "\\`")
}

#[cfg(test)]
mod tests {
    use super::super::view::{CanonicalHumanView, HumanField, HumanFieldValue};
    use super::render;

    #[test]
    fn canonical_markdown_golden_formats_only_the_supplied_view() {
        let rendered = render(CanonicalHumanView {
            heading: "feedback_list".to_owned(),
            fields: vec![
                HumanField {
                    label: "Payload",
                    value: HumanFieldValue::Block("src/lib.rs:1-1\nfn main() {}".to_owned()),
                },
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
            "## feedback\\_list\n\n### Payload\n\n    src/lib.rs:1-1\n    fn main() {}\n\n- Status: `problem`\n- Message: Retry \\*after\\* refresh"
        );
    }
}
