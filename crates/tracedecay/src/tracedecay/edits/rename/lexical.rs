fn delimiter_span_contains(line: &str, start: usize, delimiter: u8) -> bool {
    let bytes = line.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != delimiter {
            cursor += 1;
            continue;
        }
        let opening = cursor;
        cursor += 1;
        let mut escaped = false;
        while cursor < bytes.len() {
            let byte = bytes[cursor];
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                if opening < start && start < cursor {
                    return true;
                }
                cursor += 1;
                break;
            }
            cursor += 1;
        }
    }
    false
}

fn rust_char_literal_at(line: &str, start: usize) -> bool {
    let bytes = line.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'\'' {
            cursor += 1;
            continue;
        }
        let opening = cursor;
        cursor += 1;
        let mut escaped = false;
        while cursor < bytes.len() {
            let byte = bytes[cursor];
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'\'' {
                let content = &line[opening + 1..cursor];
                let valid_char = content.chars().count() == 1 || content.starts_with('\\');
                if valid_char && opening < start && start < cursor {
                    return true;
                }
                cursor += 1;
                break;
            }
            cursor += 1;
        }
    }
    false
}

pub(crate) fn string_literal_at(line: &str, start: usize, path: &str) -> bool {
    delimiter_span_contains(line, start, b'"')
        || if path.ends_with(".rs") {
            rust_char_literal_at(line, start)
        } else {
            delimiter_span_contains(line, start, b'\'')
        }
        || (!path.ends_with(".rs") && delimiter_span_contains(line, start, b'`'))
}

pub(crate) fn is_valid_identifier(name: &str, path: &str) -> bool {
    let javascript = [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs"]
        .into_iter()
        .any(|extension| path.ends_with(extension));
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_alphabetic() || (javascript && first == '$'))
        && chars.all(|character| {
            character == '_' || character.is_alphanumeric() || (javascript && character == '$')
        })
}

#[cfg(test)]
mod tests {
    use super::{is_valid_identifier, string_literal_at};

    #[test]
    fn recognizes_supported_protected_literal_forms() {
        for (line, token, path) in [
            ("const value = \"old_name\";", "old_name", "src/model.rs"),
            ("const value = 'old_name';", "old_name", "src/model.ts"),
            ("const value = `old_name`;", "old_name", "src/model.ts"),
            ("const value = 'o';", "o", "src/model.rs"),
        ] {
            assert!(string_literal_at(line, line.rfind(token).unwrap(), path));
        }
    }

    #[test]
    fn rust_lifetimes_remain_code() {
        let line = "fn borrow<'old_name>(value: &'old_name str) {}";
        for start in line.match_indices("old_name").map(|(start, _)| start) {
            assert!(!string_literal_at(line, start, "src/model.rs"));
        }
    }

    #[test]
    fn identifiers_are_conservative_and_language_aware() {
        assert!(is_valid_identifier("renamed_symbol", "src/model.rs"));
        assert!(is_valid_identifier("$renamed", "src/model.ts"));
        assert!(!is_valid_identifier("$renamed", "src/model.rs"));
        assert!(!is_valid_identifier("1abc", "src/model.rs"));
        assert!(!is_valid_identifier("rename😀", "src/model.rs"));
    }
}
