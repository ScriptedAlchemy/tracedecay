//! Parser for the `SKILL.md` frontmatter subset used by plugin and managed
//! skills: fenced `key: value` scalars plus indented block values.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use crate::{Result, config_error};

/// Why a quoted YAML scalar could not be decoded.
#[derive(Debug)]
pub enum YamlScalarError {
    /// A `'`-quoted scalar is unterminated or contains an unescaped quote.
    MalformedSingleQuoted,
    /// A `"`-quoted scalar is not valid JSON string syntax. The cause names
    /// the offending escape or position, which a caller reporting the failure
    /// to a user needs and cannot reconstruct from the variant alone.
    MalformedDoubleQuoted(serde_json::Error),
}

impl fmt::Display for YamlScalarError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedSingleQuoted => {
                formatter.write_str("malformed single-quoted YAML scalar")
            }
            Self::MalformedDoubleQuoted(error) => {
                write!(formatter, "malformed double-quoted YAML scalar: {error}")
            }
        }
    }
}

impl Error for YamlScalarError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MalformedSingleQuoted => None,
            Self::MalformedDoubleQuoted(error) => Some(error),
        }
    }
}

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

/// Strips one level of YAML quoting from an inline scalar.
///
/// Plain scalars and the borrowed interior of a simple `'`-quoted scalar are
/// returned without allocating. Applies no schema or policy — a caller decides
/// what the decoded text means.
pub fn decode_yaml_scalar(value: &str) -> std::result::Result<Cow<'_, str>, YamlScalarError> {
    if let Some(quoted) = value.strip_prefix('\'') {
        let Some(inner) = quoted.strip_suffix('\'') else {
            return Err(YamlScalarError::MalformedSingleQuoted);
        };

        let mut chars = inner.chars().peekable();
        let mut has_doubled_quote = false;
        while let Some(ch) = chars.next() {
            if ch == '\'' {
                if chars.next() != Some('\'') {
                    return Err(YamlScalarError::MalformedSingleQuoted);
                }
                has_doubled_quote = true;
            }
        }

        return Ok(if has_doubled_quote {
            Cow::Owned(inner.replace("''", "'"))
        } else {
            Cow::Borrowed(inner)
        });
    }

    if value.starts_with('"') {
        return serde_json::from_str::<String>(value)
            .map(Cow::Owned)
            .map_err(YamlScalarError::MalformedDoubleQuoted);
    }

    Ok(Cow::Borrowed(value))
}

fn unquote_scalar(value: &str) -> String {
    match decode_yaml_scalar(value) {
        Ok(decoded) => decoded.into_owned(),
        Err(YamlScalarError::MalformedSingleQuoted) => value
            .strip_prefix('\'')
            .and_then(|quoted| quoted.strip_suffix('\''))
            .map_or_else(|| value.to_string(), |inner| inner.replace("''", "'")),
        Err(YamlScalarError::MalformedDoubleQuoted(_)) => value
            .strip_prefix('"')
            .and_then(|quoted| quoted.strip_suffix('"'))
            .unwrap_or(value)
            .to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::borrow::Cow;

    use super::{
        SkillFrontmatterValue, YamlScalarError, decode_yaml_scalar, parse_skill_frontmatter,
        unquote_scalar,
    };

    #[test]
    fn borrows_plain_scalars_without_applying_policy() {
        assert!(matches!(
            decode_yaml_scalar("plain value"),
            Ok(Cow::Borrowed("plain value"))
        ));
        assert!(matches!(decode_yaml_scalar(""), Ok(Cow::Borrowed(""))));
        assert!(matches!(
            decode_yaml_scalar(" users'"),
            Ok(Cow::Borrowed(" users'"))
        ));
    }

    #[test]
    fn decodes_single_quoted_scalars_and_doubled_quotes() {
        assert!(matches!(
            decode_yaml_scalar("'plain'"),
            Ok(Cow::Borrowed("plain"))
        ));
        let decoded = decode_yaml_scalar("'it''s YAML'").expect("valid single-quoted scalar");
        assert_eq!(decoded, "it's YAML");
    }

    #[test]
    fn decodes_json_compatible_double_quoted_scalars() {
        let decoded = decode_yaml_scalar(r#""line\n☺""#).expect("valid double-quoted scalar");
        assert_eq!(decoded, "line\n☺");
    }

    #[test]
    fn rejects_malformed_single_quoted_scalars() {
        for value in ["'", "'unterminated", "'isn't valid'", "'triple'''quote'"] {
            assert!(matches!(
                decode_yaml_scalar(value),
                Err(YamlScalarError::MalformedSingleQuoted)
            ));
        }
    }

    #[test]
    fn rejects_malformed_double_quoted_scalars_and_keeps_the_cause() {
        for value in [
            r#""unterminated"#,
            r#""bad\xescape""#,
            r#""value" trailing"#,
        ] {
            let error = decode_yaml_scalar(value).expect_err("malformed double-quoted scalar");
            assert!(matches!(error, YamlScalarError::MalformedDoubleQuoted(_)));
            assert!(
                std::error::Error::source(&error).is_some(),
                "the serde cause is what a reporting caller shows the user"
            );
        }
    }

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
    fn shared_decoder_parity_preserves_successful_quote_mechanics() {
        for (value, expected) in [
            ("'plain'", "plain"),
            ("'it''s YAML'", "it's YAML"),
            (r#""line\n☺""#, "line\n☺"),
        ] {
            assert_eq!(unquote_scalar(value), expected);
        }
    }

    #[test]
    fn preserves_malformed_quote_and_plain_scalar_policy() {
        for (value, expected) in [
            (r#""bad\xescape""#, r"bad\xescape"),
            (r#""unterminated"#, r#""unterminated"#),
            ("'isn't valid'", "isn't valid"),
            ("'unterminated", "'unterminated"),
            ("", ""),
            ("plain value", "plain value"),
        ] {
            assert_eq!(unquote_scalar(value), expected);
        }
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
