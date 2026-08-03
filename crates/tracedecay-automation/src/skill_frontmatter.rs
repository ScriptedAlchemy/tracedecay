//! Parser for the `SKILL.md` frontmatter subset used by plugin and managed
//! skills: fenced `key: value` scalars plus indented block values.

use std::collections::BTreeMap;

use crate::error::{Result, config_error};

/// One parsed frontmatter value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillFrontmatterValue {
    /// Inline scalar with one level of YAML quotes stripped.
    Scalar(String),
    /// Trimmed block lines under a key with no inline value.
    Block(Vec<String>),
}

impl SkillFrontmatterValue {
    pub fn as_scalar(&self) -> Option<&str> {
        match self {
            Self::Scalar(scalar) => Some(scalar),
            Self::Block(_) => None,
        }
    }

    /// Returns unquoted `- item` entries when every block line is a list item.
    pub fn as_list_items(&self) -> Option<Vec<String>> {
        match self {
            Self::Scalar(_) => None,
            Self::Block(lines) => {
                if lines.is_empty() {
                    return None;
                }
                lines
                    .iter()
                    .map(|line| {
                        line.strip_prefix("- ")
                            .map(|item| unquote_scalar(item.trim()))
                    })
                    .collect()
            }
        }
    }
}

/// Parses leading `---`-fenced frontmatter, normalizing LF and CRLF input.
pub fn parse_skill_frontmatter(contents: &str) -> Result<BTreeMap<String, SkillFrontmatterValue>> {
    let mut lines = contents.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err(config_error("must start with YAML frontmatter"));
    }

    let mut fields: BTreeMap<String, SkillFrontmatterValue> = BTreeMap::new();
    let mut last_key: Option<String> = None;
    let mut closed = false;
    for line in lines {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            let Some(key) = &last_key else {
                return Err(config_error(format!(
                    "has indented frontmatter line before any key: {line:?}"
                )));
            };
            let Some(value) = fields.get_mut(key) else {
                return Err(config_error(format!(
                    "lost track of frontmatter key {key} while parsing a block"
                )));
            };
            match value {
                SkillFrontmatterValue::Block(block_lines) => {
                    block_lines.push(line.trim().to_string());
                }
                SkillFrontmatterValue::Scalar(_) => {
                    return Err(config_error(format!(
                        "key {key} mixes an inline scalar with block continuation lines"
                    )));
                }
            }
            continue;
        }
        let Some((key, raw_value)) = line.split_once(':') else {
            return Err(config_error(format!(
                "has invalid frontmatter line {line:?}"
            )));
        };
        let key = key.trim().to_string();
        let raw_value = raw_value.trim();
        let value = if raw_value.is_empty() {
            SkillFrontmatterValue::Block(Vec::new())
        } else {
            SkillFrontmatterValue::Scalar(unquote_scalar(raw_value))
        };
        if fields.insert(key.clone(), value).is_some() {
            return Err(config_error(format!("duplicates frontmatter key {key}")));
        }
        last_key = Some(key);
    }
    if !closed {
        return Err(config_error("must close YAML frontmatter"));
    }
    Ok(fields)
}

fn unquote_scalar(value: &str) -> String {
    if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        inner.replace("''", "'")
    } else if let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        serde_json::from_str(value).unwrap_or_else(|_| inner.to_string())
    } else {
        value.to_string()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{SkillFrontmatterValue, parse_skill_frontmatter};

    #[test]
    fn parses_scalars_blocks_and_quoting() {
        let doc = concat!(
            "---\n",
            "name: my-skill\n",
            "description: 'Use when a branch''s graph is compared.'\n",
            "paths:\n",
            "  - \"**/*.rs\"\n",
            "  - \"**/Cargo.toml\"\n",
            "---\n",
            "\n# Body\n"
        );
        let fields = parse_skill_frontmatter(doc).unwrap();
        assert_eq!(fields["name"].as_scalar(), Some("my-skill"));
        assert_eq!(
            fields["description"].as_scalar(),
            Some("Use when a branch's graph is compared."),
            "single-quote doubling must be unescaped"
        );
        assert_eq!(
            fields["paths"].as_list_items(),
            Some(vec!["**/*.rs".to_string(), "**/Cargo.toml".to_string()])
        );
    }

    #[test]
    fn parses_json_escaped_double_quoted_scalars() {
        let doc = concat!(
            "---\n",
            "name: my-skill\n",
            "description: \"Use \\\"quoted\\\" paths like C:\\\\tmp\"\n",
            "paths:\n",
            "  - \"C:\\\\tmp\"\n",
            "---\n",
            "\n# Body\n"
        );
        let fields = parse_skill_frontmatter(doc).unwrap();
        assert_eq!(
            fields["description"].as_scalar(),
            Some(r#"Use "quoted" paths like C:\tmp"#)
        );
        assert_eq!(
            fields["paths"].as_list_items(),
            Some(vec![r"C:\tmp".to_string()])
        );
    }

    /// GitHub Windows runners check out with `core.autocrlf=true`, so every
    /// SKILL.md arrives with CRLF line endings; parsing must not depend on
    /// exact `---\n` byte sequences.
    #[test]
    fn parses_crlf_documents_identically_to_lf() {
        let lf = "---\nname: my-skill\ndescription: Use when testing.\npaths:\n  - \"**/*.rs\"\n---\n\n# Body\n";
        let crlf = lf.replace('\n', "\r\n");
        assert_eq!(
            parse_skill_frontmatter(&crlf).unwrap(),
            parse_skill_frontmatter(lf).unwrap()
        );
    }

    #[test]
    fn nested_maps_are_blocks_but_not_list_items() {
        let doc = "---\nname: my-skill\nmetadata:\n  author: someone\n---\nBody\n";
        let fields = parse_skill_frontmatter(doc).unwrap();
        assert_eq!(
            fields["metadata"],
            SkillFrontmatterValue::Block(vec!["author: someone".to_string()])
        );
        assert_eq!(fields["metadata"].as_list_items(), None);
        assert_eq!(fields["metadata"].as_scalar(), None);
    }

    #[test]
    fn rejects_malformed_frontmatter() {
        for (doc, reason) in [
            ("# no frontmatter\n", "missing opening fence"),
            ("---\nname: x\n", "unclosed frontmatter"),
            ("---\nname: x\nname: y\n---\n", "duplicate key"),
            ("---\njust some text\n---\n", "non-mapping line"),
            (
                "---\n  - orphan\nname: x\n---\n",
                "indented line before any key",
            ),
            (
                "---\nname: x\n  - continuation\n---\n",
                "block continuation under an inline scalar",
            ),
        ] {
            assert!(
                parse_skill_frontmatter(doc).is_err(),
                "expected parse error for {reason}: {doc:?}"
            );
        }
    }
}
