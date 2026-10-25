//! Cook string values from the existing JavaScript/TypeScript syntax tree.
//!
//! Tree-sitter owns token boundaries and identifies substitutions. This layer
//! decodes its literal fragments and escape tokens; it never evaluates source.

use tree_sitter::Node as TsNode;

pub(super) fn string_literal_value(
    source: &[u8],
    node: TsNode<'_>,
) -> Result<Option<String>, String> {
    let template = match node.kind() {
        "string" => false,
        "template_string" => true,
        _ => return Ok(None),
    };
    if node.has_error() || node.is_missing() {
        return Err("string literal contains syntax errors".to_owned());
    }
    let mut cursor = node.walk();
    if template
        && node
            .named_children(&mut cursor)
            .any(|child| child.kind() == "template_substitution")
    {
        return Ok(None);
    }

    // JavaScript strings contain UTF-16 code units. A pair of adjacent \uXXXX
    // escapes must be combined before converting to Rust's Unicode strings;
    // an unpaired surrogate cannot name an exact UTF-8 module path.
    let mut units = Vec::new();
    for child in node.named_children(&mut cursor) {
        let text = child
            .utf8_text(source)
            .map_err(|error| format!("string literal is not UTF-8: {error}"))?;
        match child.kind() {
            "string_fragment" if template => {
                let mut chars = text.chars().peekable();
                while let Some(ch) = chars.next() {
                    if ch == '\r' {
                        if chars.peek() == Some(&'\n') {
                            chars.next();
                        }
                        units.push(u16::from(b'\n'));
                    } else {
                        units.extend_from_slice(ch.encode_utf16(&mut [0; 2]));
                    }
                }
            }
            "string_fragment" => units.extend(text.encode_utf16()),
            "escape_sequence" => {
                append_escape(text, source.get(child.end_byte()).copied(), &mut units)?;
            }
            other => {
                return Err(format!("unsupported string literal syntax: {other}"));
            }
        }
    }
    String::from_utf16(&units)
        .map(Some)
        .map_err(|_| "string literal contains an unpaired UTF-16 surrogate".to_owned())
}

fn append_escape(
    text: &str,
    following_byte: Option<u8>,
    units: &mut Vec<u16>,
) -> Result<(), String> {
    let Some(escape) = text.strip_prefix('\\') else {
        return Err("escape token has no backslash".to_owned());
    };
    let code_unit = match escape {
        "\n" | "\r" | "\r\n" | "\u{2028}" | "\u{2029}" => return Ok(()),
        "b" => 0x0008,
        "t" => 0x0009,
        "n" => 0x000a,
        "v" => 0x000b,
        "f" => 0x000c,
        "r" => 0x000d,
        "0" if !following_byte.is_some_and(|byte| byte.is_ascii_digit()) => 0,
        _ if escape.starts_with(|ch: char| ch.is_ascii_digit()) => {
            // The permissive grammar also accepts sloppy-mode octal and
            // decimal escapes. Do not guess strict/sloppy context, or silently
            // read their spelling as a path. Callers report partial coverage.
            return Err("legacy octal and decimal string escapes are unsupported".to_owned());
        }
        _ if escape.starts_with('x') && escape.len() == 3 => u16::from_str_radix(&escape[1..], 16)
            .map_err(|_| "invalid hexadecimal string escape".to_owned())?,
        _ if escape.starts_with("u{") && escape.ends_with('}') => {
            let point = u32::from_str_radix(&escape[2..escape.len() - 1], 16)
                .map_err(|_| "invalid Unicode code point escape".to_owned())?;
            if let Ok(unit) = u16::try_from(point) {
                units.push(unit);
                return Ok(());
            }
            let Some(ch) = char::from_u32(point) else {
                return Err("Unicode string escape exceeds the code point range".to_owned());
            };
            units.extend_from_slice(ch.encode_utf16(&mut [0; 2]));
            return Ok(());
        }
        _ if escape.starts_with('u') && escape.len() == 5 => u16::from_str_radix(&escape[1..], 16)
            .map_err(|_| "invalid Unicode string escape".to_owned())?,
        _ => {
            let mut chars = escape.chars();
            let Some(ch) = chars.next() else {
                return Err("empty string escape".to_owned());
            };
            if chars.next().is_some() || matches!(ch, 'x' | 'u') {
                return Err("invalid string escape".to_owned());
            }
            units.extend_from_slice(ch.encode_utf16(&mut [0; 2]));
            return Ok(());
        }
    };
    units.push(code_unit);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::TypeScriptExtractor;
    use crate::traversal::find_descendant_by_kind;

    use super::string_literal_value;

    fn decode(literal: &str) -> Result<Option<String>, String> {
        let source = format!("const value = {literal};");
        let tree = TypeScriptExtractor::parse_source(&source, "ts").expect("parse fixture");
        let literal = find_descendant_by_kind(tree.root_node(), "string")
            .or_else(|| find_descendant_by_kind(tree.root_node(), "template_string"))
            .expect("fixture has a literal node");
        string_literal_value(source.as_bytes(), literal)
    }

    #[test]
    fn parser_literals_decode_escapes_and_unicode_as_values() {
        for (literal, expected) in [
            (r#""./es\x63aped""#, "./escaped"),
            (r#"'./\u0063afé'"#, "./café"),
            (r#"'./quo\'ted'"#, "./quo'ted"),
            (r#""./back\\slash""#, "./back\\slash"),
            (r#""./\uD83D\uDE80""#, "./🚀"),
            (r#"`./\u{1F680}-lazy`"#, "./🚀-lazy"),
            (r#""./\u{D83D}\u{DE80}""#, "./🚀"),
            (r#""\0x""#, "\0x"),
            (r#""\b\t\n\v\f\r""#, "\u{8}\t\n\u{b}\u{c}\r"),
            (r#""\q\/\$""#, "q/$"),
            ("''", ""),
            ("``", ""),
        ] {
            assert_eq!(decode(literal), Ok(Some(expected.to_owned())), "{literal}");
        }
    }

    #[test]
    fn parser_literals_cook_line_continuations_and_template_newlines() {
        for newline in ["\n", "\r\n", "\r", "\u{2028}", "\u{2029}"] {
            let literal = format!("'./line\\{newline}continued'");
            assert_eq!(decode(&literal), Ok(Some("./linecontinued".to_owned())));
        }
        assert_eq!(
            decode("`first\r\nsecond\rthird`"),
            Ok(Some("first\nsecond\nthird".to_owned()))
        );
        assert_eq!(decode(r#"`\${name}`"#), Ok(Some("${name}".to_owned())));
    }

    #[test]
    fn substitutions_are_expressions_and_invalid_values_have_coverage_errors() {
        assert_eq!(decode("`./${name}`"), Ok(None));
        for literal in [
            r#"'./\uD800'"#,
            r#"'./\uDC00'"#,
            r#"'./\u{110000}'"#,
            r#"'./\056legacy'"#,
            r#"'./\08legacy'"#,
            r#"'./\09legacy'"#,
            r#"'./\8legacy'"#,
            r#"'./\9legacy'"#,
        ] {
            assert!(decode(literal).is_err(), "{literal}");
        }
    }
}
