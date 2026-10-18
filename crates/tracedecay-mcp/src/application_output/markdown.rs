//! Compact human-view contract for CLI and MCP presenters.

use super::view::{CanonicalHumanView, HumanFieldValue, PER_CALL_FIELDS};
use crate::tools::render::MarkdownFence;

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

#[tracing::instrument(name = "mcp.application_output.render", level = "trace", skip_all)]
pub fn render(view: CanonicalHumanView) -> MarkdownView {
    let mut text = format!("## {}\n", escape_text(&view.heading));
    for field in view.fields {
        match field.value {
            HumanFieldValue::Markdown(value) => {
                text.push_str("\n### ");
                text.push_str(field.label);
                text.push_str("\n\n");
                text.push_str(&value);
                if !value.ends_with('\n') {
                    text.push('\n');
                }
            }
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

/// `text` without its rendered per-call field lines, which differ on every
/// call for the same answer. Every other byte, line terminators included, is
/// kept. Per-call fields exist only in canonical application views, the only
/// renderers that emit `## ` heading lines; in any other text the same line
/// shape is payload and every byte is kept. Lines inside fenced code are
/// payload too, such as a source body that itself holds a `- Request: ` line.
pub(crate) fn without_per_call_lines(text: &str) -> String {
    if !text.starts_with("## ") && !text.contains("\n## ") {
        return text.to_owned();
    }
    let mut fence = MarkdownFence::default();
    text.split_inclusive('\n')
        .filter(|line| {
            let fenced = fence.is_open();
            fence.observe(line);
            fenced || !is_per_call_line(line)
        })
        .collect()
}

/// Whether `line` is a rendered per-call field: a `Code` value renders its
/// value as a single inline code span with escaped backticks, so a payload
/// line like `- Request: a plain note` does not match.
fn is_per_call_line(line: &str) -> bool {
    let Some(field) = line.trim_end_matches(['\r', '\n']).strip_prefix("- ") else {
        return false;
    };
    PER_CALL_FIELDS.iter().any(|label| {
        field
            .strip_prefix(label)
            .and_then(|value| value.strip_prefix(": `"))
            .is_some_and(|value| {
                value.ends_with('`') && !value[..value.len() - 1].replace("\\`", "").contains('`')
            })
    })
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
    use super::super::view::{
        CanonicalHumanView, HumanField, HumanFieldValue, PROVENANCE_FIELD, RECEIPT_FIELD,
        REQUEST_FIELD, TRACE_FIELD,
    };
    use super::{render, without_per_call_lines};

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

    #[test]
    fn per_call_lines_are_only_the_per_call_fields() {
        let rendered = render(CanonicalHumanView {
            heading: "source_body".to_owned(),
            fields: vec![
                HumanField {
                    label: "Payload",
                    value: HumanFieldValue::Block("- Provenance: `in the source`".to_owned()),
                },
                HumanField {
                    label: "Body",
                    value: HumanFieldValue::Markdown(
                        "```text\r\n- Request: `in the source`\r\n```\r\n".to_owned(),
                    ),
                },
                HumanField {
                    label: PROVENANCE_FIELD,
                    value: HumanFieldValue::Code("request=r1; elapsed_us=9".to_owned()),
                },
                HumanField {
                    label: RECEIPT_FIELD,
                    value: HumanFieldValue::Code("started=1, ended=2".to_owned()),
                },
                HumanField {
                    label: "Receipt operation",
                    value: HumanFieldValue::Code("read".to_owned()),
                },
                HumanField {
                    label: REQUEST_FIELD,
                    value: HumanFieldValue::Code("r1".to_owned()),
                },
                HumanField {
                    label: TRACE_FIELD,
                    value: HumanFieldValue::Code("t1".to_owned()),
                },
            ],
        });
        assert_eq!(
            without_per_call_lines(rendered.as_str()),
            "## source\\_body\n\n### Payload\n\n    - Provenance: `in the source`\n\n### Body\n\n\
             ```text\r\n- Request: `in the source`\r\n```\r\n\n- Receipt operation: `read`\n"
        );
    }

    #[test]
    fn per_call_shaped_lines_in_other_views_are_kept() {
        // `# Context for {task}` inserts the task verbatim, and its memory
        // lines are `- ` bullets; none of it is a per-call field even when a
        // line happens to carry a per-call label.
        let context =
            "# Context for explain `- Request: `\n- Request: `a fact about r1`\n- Trace: tail\n";
        assert_eq!(without_per_call_lines(context), context);

        let rendered = render(CanonicalHumanView {
            heading: "context".to_owned(),
            fields: vec![
                HumanField {
                    label: "Memory",
                    value: HumanFieldValue::Markdown(
                        "- Request: a plain note\n- Trace: tail\n".to_owned(),
                    ),
                },
                HumanField {
                    label: PROVENANCE_FIELD,
                    value: HumanFieldValue::Code("request=r1".to_owned()),
                },
            ],
        });
        assert_eq!(
            without_per_call_lines(rendered.as_str()),
            "## context\n\n### Memory\n\n- Request: a plain note\n- Trace: tail\n\n"
        );
    }
}
