//! Canonical parser for `SKILL.md` YAML frontmatter.
//!
//! Skill frontmatter across the repo (bundled `codex-plugin/` and
//! `cursor-plugin/` skills, Hermes hub skills, agent-managed exports) uses a
//! small YAML subset: a `---` fence, `key: value` scalars (optionally single-
//! or double-quoted), and block values made of indented lines (list items or
//! nested maps). This module is the one place that subset is parsed so
//! consumers ([`crate::automation::hermes_skill_inventory`] and the plugin
//! contract tests in `tests/plugin_skill_contract_test.rs`) stop growing
//! bespoke, subtly different parsers.
//!
//! Parsing is line-ending tolerant: CRLF checkouts (e.g. GitHub Windows
//! runners with `core.autocrlf=true`) parse identically to LF checkouts.

use std::collections::BTreeMap;

use crate::errors::{Result, TraceDecayError};

/// One frontmatter value: either an inline scalar (`key: value`) or a block
/// of indented continuation lines (`key:` followed by list items or nested
/// mappings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillFrontmatterValue {
    /// Inline scalar with quoting already resolved (outer quotes stripped,
    /// YAML `''` doubling inside single-quoted scalars unescaped).
    Scalar(String),
    /// Raw trimmed block lines under a key with no inline value.
    Block(Vec<String>),
}

impl SkillFrontmatterValue {
    pub fn as_scalar(&self) -> Option<&str> {
        match self {
            Self::Scalar(scalar) => Some(scalar),
            Self::Block(_) => None,
        }
    }

    /// Returns the unquoted items of a block whose every line is a YAML list
    /// item (`- item`), or `None` for scalars and other block shapes (nested
    /// maps, empty blocks).
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

/// Parses the leading `---`-fenced YAML frontmatter of a `SKILL.md` document.
///
/// Returns an error when the document does not open with frontmatter, never
/// closes it, repeats a key, or contains a top-level line that is not a
/// `key: value` mapping. Line endings (`\n` vs `\r\n`) are normalized away.
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

/// Strips one level of YAML quoting: single-quoted scalars also unescape the
/// `''` doubling YAML uses to embed a literal `'`. Skill frontmatter never
/// uses backslash escapes, so double-quoted scalars only lose their quotes.
fn unquote_scalar(value: &str) -> String {
    if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        inner.replace("''", "'")
    } else if let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        inner.to_string()
    } else {
        value.to_string()
    }
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{parse_skill_frontmatter, SkillFrontmatterValue};

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
