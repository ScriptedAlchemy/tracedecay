use std::collections::HashSet;

pub fn normalize_entity(entity: &str) -> String {
    entity
        .trim_matches(|c: char| {
            c.is_ascii_punctuation() && c != '_' && c != '/' && c != '\\' && c != ':' && c != '.'
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn extract_entities(text: &str) -> Vec<String> {
    let mut matches = Vec::new();

    matches.extend(extract_quoted(text, '"'));
    matches.extend(extract_quoted(text, '\''));
    matches.extend(extract_aliases(text));
    matches.extend(extract_code_tokens(text));
    matches.extend(extract_capitalized_names(text));

    matches.sort_by_key(|(index, _)| *index);

    let mut seen = HashSet::new();
    let mut entities = Vec::new();
    for (_, entity) in matches {
        let normalized = normalize_entity(&entity);
        if normalized.is_empty() {
            continue;
        }

        let key = normalized.to_ascii_lowercase();
        if seen.insert(key) {
            entities.push(normalized);
        }
    }

    entities
}

fn extract_quoted(text: &str, delimiter: char) -> Vec<(usize, String)> {
    let mut results = Vec::new();
    let mut start = None;

    for (index, ch) in text.char_indices() {
        if ch != delimiter {
            continue;
        }

        if let Some(open_index) = start {
            let content_start = open_index + delimiter.len_utf8();
            if content_start < index {
                results.push((content_start, text[content_start..index].to_string()));
            }
            start = None;
        } else {
            start = Some(index);
        }
    }

    results
}

fn extract_aliases(text: &str) -> Vec<(usize, String)> {
    let lower = text.to_ascii_lowercase();
    [" aka ", " a.k.a. ", " also known as "]
        .into_iter()
        .flat_map(|marker| {
            lower
                .match_indices(marker)
                .filter_map(move |(index, matched)| {
                    let phrase_start = index + matched.len();
                    let phrase = take_entity_phrase(&text[phrase_start..]);
                    if phrase.is_empty() {
                        None
                    } else {
                        Some((phrase_start, phrase))
                    }
                })
        })
        .collect()
}

fn take_entity_phrase(text: &str) -> String {
    let trimmed_start = text.len() - text.trim_start().len();
    let remaining = &text[trimmed_start..];
    let mut end = remaining.len();

    for (index, _) in remaining.char_indices() {
        let rest = &remaining[index..];
        if index > 0 && (rest.starts_with(" in ") || rest.starts_with(" via ")) {
            end = index;
            break;
        }

        if let Some(ch) = rest.chars().next() {
            if matches!(ch, ',' | '.' | ';' | '"' | '\'') {
                end = index;
                break;
            }
        }
    }

    normalize_entity(&remaining[..end])
}

fn extract_code_tokens(text: &str) -> Vec<(usize, String)> {
    token_spans(text)
        .into_iter()
        .filter_map(|(index, token)| {
            let cleaned = clean_code_token(token);
            if is_file_path(&cleaned) || is_rust_symbol(&cleaned) || is_tracedecay_tool(&cleaned) {
                Some((index, cleaned))
            } else {
                None
            }
        })
        .collect()
}

fn extract_capitalized_names(text: &str) -> Vec<(usize, String)> {
    let mut results = Vec::new();
    let mut current = Vec::new();
    let mut start_index = 0;

    for (index, token) in token_spans(text) {
        let word = clean_name_token(token);
        if is_capitalized_word(&word) {
            if current.is_empty() {
                start_index = index;
            }
            current.push(word);
        } else {
            push_capitalized_sequence(&mut results, start_index, &mut current);
        }
    }

    push_capitalized_sequence(&mut results, start_index, &mut current);
    results
}

fn push_capitalized_sequence(
    results: &mut Vec<(usize, String)>,
    start_index: usize,
    current: &mut Vec<String>,
) {
    if current.len() >= 2 && !is_non_entity_leading_word(&current[0]) {
        results.push((start_index, current.join(" ")));
    }
    current.clear();
}

fn is_non_entity_leading_word(token: &str) -> bool {
    matches!(
        token,
        "Add"
            | "Avoid"
            | "Create"
            | "Delete"
            | "Do"
            | "Fix"
            | "Implement"
            | "Keep"
            | "Persist"
            | "Prefer"
            | "Record"
            | "Remove"
            | "Update"
            | "Use"
    )
}

fn token_spans(text: &str) -> Vec<(usize, &str)> {
    let mut spans = Vec::new();
    let mut offset = 0;

    for token in text.split_whitespace() {
        if let Some(relative_index) = text[offset..].find(token) {
            let index = offset + relative_index;
            spans.push((index, token));
            offset = index + token.len();
        }
    }

    spans
}

fn clean_code_token(token: &str) -> String {
    let cleaned = token
        .trim_matches(|c: char| {
            c.is_ascii_punctuation()
                && c != '_'
                && c != '/'
                && c != '\\'
                && c != '.'
                && c != ':'
                && c != '-'
        })
        .trim_end_matches("()")
        .to_string();

    let normalized_tool = cleaned.replace('-', "_").to_ascii_lowercase();
    // Accept both tracedecay_ (new) and tokensave_ (legacy) tool name prefixes.
    if normalized_tool.starts_with("tracedecay_") || normalized_tool.starts_with("tokensave_") {
        normalized_tool.trim_end_matches('.').to_string()
    } else {
        cleaned.trim_end_matches('.').to_string()
    }
}

fn clean_name_token(token: &str) -> String {
    token
        .trim_matches(|c: char| c.is_ascii_punctuation() && c != '-' && c != '_')
        .to_string()
}

fn is_file_path(token: &str) -> bool {
    token.contains('/') || token.contains('\\') || token.starts_with('.')
}

fn is_rust_symbol(token: &str) -> bool {
    token.contains("::")
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | ':'))
}

/// Returns true for both `tracedecay_*` (new) and `tokensave_*` (legacy)
/// MCP tool names found in stored session messages.
///
/// LEGACY-COMPAT: tokensave_ prefix accepted alongside tracedecay_.
fn is_tracedecay_tool(token: &str) -> bool {
    let normalized = token.replace('-', "_").to_ascii_lowercase();
    (normalized.starts_with("tracedecay_") || normalized.starts_with("tokensave_"))
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn is_capitalized_word(token: &str) -> bool {
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    first.is_ascii_uppercase()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        && !token.contains("::")
}
