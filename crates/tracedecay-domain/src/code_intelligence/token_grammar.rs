//! The one technical token grammar shared by extraction and retrieval.
//!
//! Code-index extraction, the lexical projection's document tokenizer, and
//! the query-side parsers all consume these primitives, so a term the
//! indexer keeps whole (`foo-bar`, `a::b`, `p/q.rs`) can never be split
//! differently by the query that searches for it, and exact admission can
//! never accept a term shape the extractor would not mint.
//! `ExactTechnicalTermV1` minting validates against the same per-kind
//! recognizers (see `search::validate_self_authenticating_technical_term`).

use std::borrow::Cow;

use crate::retrieval::ExactFieldV1;

use super::search::ExactTechnicalTermKindV1;

/// Characters that extend one maximal technical token: identifiers plus the
/// path, qualifier, key, and flag separators the extractor keeps whole.
pub fn is_technical_token_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | ':' | '.' | '/')
}

/// Maximal technical tokens of `text` with their starting byte offsets.
pub fn technical_tokens(text: &str) -> impl Iterator<Item = (usize, &str)> + '_ {
    let mut chars = text.char_indices().peekable();
    std::iter::from_fn(move || {
        while chars
            .peek()
            .is_some_and(|(_, character)| !is_technical_token_char(*character))
        {
            chars.next();
        }
        let (start, _) = *chars.peek()?;
        let mut end = start;
        while let Some((index, character)) = chars.peek().copied() {
            if !is_technical_token_char(character) {
                break;
            }
            end = index + character.len_utf8();
            chars.next();
        }
        Some((start, &text[start..end]))
    })
}

/// Split one token into lowercase language-profiled subtokens: path,
/// qualifier, and key separators first, then snake/camel/digit boundaries.
pub fn split_subtokens(token: &str) -> Vec<String> {
    let mut subtokens = Vec::new();
    for segment in token.split([':', '.', '/', '-']) {
        let mut current = String::new();
        let mut prev: Option<char> = None;
        for c in segment.chars() {
            let boundary = match (prev, c) {
                (Some('_'), _) => false,
                (_, '_') => true,
                (Some(p), c) if p.is_lowercase() && c.is_uppercase() => true,
                (Some(p), c) if p.is_ascii_digit() != c.is_ascii_digit() => true,
                _ => false,
            };
            if boundary && !current.is_empty() {
                subtokens.push(current.to_lowercase());
                current.clear();
            }
            if c != '_' {
                current.push(c);
            }
            prev = Some(c);
        }
        if !current.is_empty() {
            subtokens.push(current.to_lowercase());
        }
    }
    subtokens
}

/// Classify one maximal token as a whole exact technical term kind, or
/// `None` when the token is only subtoken evidence.
pub fn classify_technical_token(token: &str) -> Option<ExactTechnicalTermKindV1> {
    if is_cli_flag_token(token) {
        return Some(ExactTechnicalTermKindV1::CliFlag);
    }
    if is_compiler_error_code_token(token) {
        return Some(ExactTechnicalTermKindV1::CompilerErrorCode);
    }
    if is_runtime_error_code_token(token) {
        return Some(ExactTechnicalTermKindV1::RuntimeErrorCode);
    }
    if is_commit_identifier_token(token) {
        return Some(ExactTechnicalTermKindV1::CommitIdentifier);
    }
    if is_tool_name_token(token) {
        return Some(ExactTechnicalTermKindV1::ToolName);
    }
    if is_qualified_name_token(token) {
        return Some(ExactTechnicalTermKindV1::QualifiedName);
    }
    if is_path_token(token) {
        return Some(ExactTechnicalTermKindV1::Path);
    }
    if is_configuration_key_token(token) {
        return Some(ExactTechnicalTermKindV1::ConfigurationKey);
    }
    None
}

/// ASCII identifier: leading alphabetic or underscore, then alphanumerics
/// and underscores.
pub fn is_identifier_token(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// `::`-qualified chain of identifiers.
pub fn is_qualified_name_token(value: &str) -> bool {
    value.contains("::") && value.split("::").all(is_identifier_token)
}

/// Slash-separated non-empty segments of identifier, hyphen, and dot
/// characters — the segment shape shared by the extraction Path grammar and
/// query-side logical-path matching (which additionally admits extensionless
/// files because every chunk's logical path is a posting).
pub fn is_path_shape(value: &str) -> bool {
    value.split('/').all(|segment| {
        !segment.is_empty()
            && segment.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
            })
    })
}

/// Extraction Path grammar: slash-separated shape whose filename carries an
/// extension dot, so bare prose ratios like `docs/readme` stay subtokens.
pub fn is_path_token(value: &str) -> bool {
    value.contains('/')
        && is_path_shape(value)
        && value
            .rsplit('/')
            .next()
            .is_some_and(|filename| filename.contains('.'))
}

/// Long-form CLI flag: `--` prefix, leading alphabetic, lowercase
/// alphanumerics and hyphens, no trailing hyphen.
pub fn is_cli_flag_token(value: &str) -> bool {
    value.strip_prefix("--").is_some_and(|flag| {
        !flag.is_empty()
            && !flag.ends_with('-')
            && flag
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphabetic())
            && flag.chars().all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
            })
    })
}

/// Compiler diagnostic code: `E`, `TS`, or `CS` followed by exactly four
/// digits.
pub fn is_compiler_error_code_token(value: &str) -> bool {
    ["E", "TS", "CS"].into_iter().any(|prefix| {
        value.strip_prefix(prefix).is_some_and(|digits| {
            digits.len() == 4 && digits.chars().all(|character| character.is_ascii_digit())
        })
    })
}

/// Runtime error code: `ERR_` followed by uppercase alphanumerics and
/// underscores.
pub fn is_runtime_error_code_token(value: &str) -> bool {
    value.strip_prefix("ERR_").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.chars().all(|character| {
                character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
            })
    })
}

/// Configuration key: three or more non-empty dotted segments of lowercase
/// alphanumerics and underscores.
pub fn is_configuration_key_token(value: &str) -> bool {
    value.split('.').count() >= 3
        && value.split('.').all(|segment| {
            !segment.is_empty()
                && segment.chars().all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
                })
        })
}

/// Known tool names recognized case-insensitively.
pub fn is_tool_name_token(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "cargo" | "rustc" | "tracedecay" | "pytest" | "kubectl" | "fastembed" | "ast-grep"
    )
}

/// Bare git object hash in the mintable abbreviation range (7..=40 hex):
/// the identifier part of a `commit:`-prefixed extraction term and the
/// query-side bare form (the exact projection strips the prefix from
/// posting keys).
pub fn is_commit_hash(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.chars().all(|character| character.is_ascii_hexdigit())
}

/// Extraction commit-identifier grammar: `commit:` plus a mintable hash.
pub fn is_commit_identifier_token(value: &str) -> bool {
    value.strip_prefix("commit:").is_some_and(is_commit_hash)
}

/// Posting-key canonical form of one exact search literal. The lexical
/// projection derives its exact posting keys and the query-side admission
/// authority derives its canonical bytes from this one mapping, so the two
/// sides of the posting map cannot diverge.
pub fn exact_search_canonical(field: ExactFieldV1, value: &str) -> Cow<'_, str> {
    match field {
        ExactFieldV1::CliFlag
        | ExactFieldV1::ToolName
        | ExactFieldV1::ConfigurationKey
        | ExactFieldV1::CommitIdentifier => {
            if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
                Cow::Owned(value.to_ascii_lowercase())
            } else {
                Cow::Borrowed(value)
            }
        }
        ExactFieldV1::DiagnosticCode => {
            if value.bytes().any(|byte| byte.is_ascii_lowercase()) {
                Cow::Owned(value.to_ascii_uppercase())
            } else {
                Cow::Borrowed(value)
            }
        }
        ExactFieldV1::Identifier
        | ExactFieldV1::QualifiedName
        | ExactFieldV1::Path
        | ExactFieldV1::QuotedPhrase
        | ExactFieldV1::DiagnosticText
        | ExactFieldV1::CompilerOrRuntimeError
        | ExactFieldV1::TaskOrSessionId
        | ExactFieldV1::ProtocolField => Cow::Borrowed(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separator_terms_stay_single_tokens() {
        let text = "use foo-bar, a::b and p/q.rs (--flag) E0308 TS1234 ERR_X!";
        let tokens: Vec<&str> = technical_tokens(text).map(|(_, token)| token).collect();
        assert_eq!(
            tokens,
            [
                "use", "foo-bar", "a::b", "and", "p/q.rs", "--flag", "E0308", "TS1234", "ERR_X"
            ]
        );
        for (offset, token) in technical_tokens(text) {
            assert_eq!(&text[offset..offset + token.len()], token);
        }
    }

    #[test]
    fn classification_matches_extraction_grammar() {
        assert_eq!(classify_technical_token("foo-bar"), None);
        assert_eq!(
            classify_technical_token("a::b"),
            Some(ExactTechnicalTermKindV1::QualifiedName)
        );
        assert_eq!(
            classify_technical_token("p/q.rs"),
            Some(ExactTechnicalTermKindV1::Path)
        );
        assert_eq!(
            classify_technical_token("--flag"),
            Some(ExactTechnicalTermKindV1::CliFlag)
        );
        assert_eq!(
            classify_technical_token("E0308"),
            Some(ExactTechnicalTermKindV1::CompilerErrorCode)
        );
        assert_eq!(
            classify_technical_token("TS1234"),
            Some(ExactTechnicalTermKindV1::CompilerErrorCode)
        );
        assert_eq!(
            classify_technical_token("ERR_X"),
            Some(ExactTechnicalTermKindV1::RuntimeErrorCode)
        );
        // Lookalikes stay subtoken-only evidence.
        assert_eq!(classify_technical_token("-v"), None);
        assert_eq!(classify_technical_token("E123"), None);
        assert_eq!(classify_technical_token("TS12345"), None);
        assert_eq!(classify_technical_token("err_x"), None);
        assert_eq!(classify_technical_token("docs/readme"), None);
        assert_eq!(classify_technical_token("a.b"), None);
        assert_eq!(classify_technical_token("deadbeef"), None);
        assert_eq!(
            classify_technical_token("commit:deadbee"),
            Some(ExactTechnicalTermKindV1::CommitIdentifier)
        );
    }

    #[test]
    fn subtokens_split_separators_snake_camel_and_digit_boundaries() {
        assert_eq!(
            split_subtokens("VectorWatermark::merge_max"),
            ["vector", "watermark", "merge", "max"]
        );
        assert_eq!(split_subtokens("p/q.rs"), ["p", "q", "rs"]);
        assert_eq!(split_subtokens("foo-bar2"), ["foo", "bar", "2"]);
    }

    #[test]
    fn posting_canonical_folds_case_per_field() {
        assert_eq!(
            exact_search_canonical(ExactFieldV1::DiagnosticCode, "e0308"),
            "E0308"
        );
        assert_eq!(
            exact_search_canonical(ExactFieldV1::CliFlag, "--Release"),
            "--release"
        );
        assert_eq!(
            exact_search_canonical(ExactFieldV1::Identifier, "MixedCase"),
            "MixedCase"
        );
        assert!(matches!(
            exact_search_canonical(ExactFieldV1::CommitIdentifier, "deadbee"),
            Cow::Borrowed("deadbee")
        ));
    }
}
