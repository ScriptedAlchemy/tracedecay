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
    if current.is_empty() {
        return;
    }

    // Strip a leading imperative/verb so it does not swallow the entity that
    // follows it. The old exact-string list missed inflections ("Prefers" vs
    // "Prefer"), so "Prefers Tokio" was captured verbatim as a phrase and
    // probe("Tokio") could never reach it.
    let mut words = current.clone();
    let stripped_verb = is_non_entity_leading_word(&words[0]);
    if stripped_verb {
        words.remove(0);
    }
    current.clear();

    if words.is_empty() {
        return;
    }

    if words.len() >= 2 {
        // Capture the remaining phrase.
        results.push((start_index, words.join(" ")));
        // When a verb led the sequence, the substantive entity is the head noun
        // of the remainder ("Avoid Foo Bar" -> "Bar"); expose it so probe can
        // reach the fact via the single noun too. Ordinary noun phrases such as
        // "Project Phoenix" keep the phrase as the entity and intentionally do
        // not emit the bare head noun, which would add noisy single-word
        // entities (Corp/Memory/Lens) and worsen retrieval Risk G.
        if stripped_verb {
            if let Some(last_word) = words.last() {
                push_single_capitalized(results, start_index, last_word);
            }
        }
        return;
    }

    // Single capitalized token. Unlike the old >=2-word rule this lets proper
    // nouns (Postgres/Tokio/Kubernetes/Database) become first-class entities;
    // common sentence-initial function words (The/This/Then/...) are filtered.
    push_single_capitalized(results, start_index, &words[0]);
}

fn push_single_capitalized(results: &mut Vec<(usize, String)>, index: usize, word: &str) {
    if word.is_empty() || is_non_entity_leading_word(word) || is_common_sentence_word(word) {
        return;
    }
    results.push((index, word.to_string()));
}

/// Returns true for imperative/leading verbs and their common inflections.
///
/// Matching is stem/inflection-based (case-insensitive) rather than a literal
/// exact-string list, so inflected forms such as "Prefers" / "Using" / "Avoided"
/// no longer leak through and swallow the entity that follows them.
/// (Retrieval-quality Risk A: the old list matched "Prefer"/"Use" only.)
fn is_non_entity_leading_word(token: &str) -> bool {
    const LEADING_VERB_FORMS: &[&str] = &[
        "add",
        "adds",
        "added",
        "adding",
        "avoid",
        "avoids",
        "avoided",
        "avoiding",
        "create",
        "creates",
        "created",
        "creating",
        "delete",
        "deletes",
        "deleted",
        "deleting",
        "do",
        "does",
        "did",
        "done",
        "doing",
        "fix",
        "fixes",
        "fixed",
        "fixing",
        "implement",
        "implements",
        "implemented",
        "implementing",
        "keep",
        "keeps",
        "kept",
        "keeping",
        "persist",
        "persists",
        "persisted",
        "persisting",
        "prefer",
        "prefers",
        "preferred",
        "preferring",
        "record",
        "records",
        "recorded",
        "recording",
        "remove",
        "removes",
        "removed",
        "removing",
        "update",
        "updates",
        "updated",
        "updating",
        "use",
        "uses",
        "used",
        "using",
    ];
    LEADING_VERB_FORMS.contains(&token.to_ascii_lowercase().as_str())
}

/// Returns true for common English function words that are capitalized only
/// because they sit at the start of a sentence (articles, pronouns, auxiliary
/// verbs, prepositions, ...). These are not entities; capturing them would add
/// noise that pollutes entity-graph retrieval (Risk G). Intentionally
/// conservative — ambiguous proper nouns such as the month "May" are excluded.
fn is_common_sentence_word(token: &str) -> bool {
    const COMMON_WORDS: &[&str] = &[
        // articles
        "a",
        "an",
        "the",
        // coordinating conjunctions
        "and",
        "but",
        "or",
        "nor",
        "so",
        "yet",
        // prepositions / subordinating conjunctions
        "as",
        "at",
        "by",
        "for",
        "from",
        "in",
        "into",
        "of",
        "on",
        "onto",
        "out",
        "over",
        "per",
        "than",
        "to",
        "under",
        "up",
        "via",
        "with",
        "without",
        // pronouns and possessives
        "he",
        "her",
        "him",
        "his",
        "i",
        "it",
        "its",
        "me",
        "my",
        "our",
        "she",
        "their",
        "them",
        "they",
        "us",
        "we",
        "you",
        "your",
        // demonstratives / deictics
        "here",
        "now",
        "that",
        "there",
        "these",
        "this",
        "those",
        // wh-words
        "how",
        "what",
        "when",
        "where",
        "which",
        "while",
        "who",
        "whom",
        "whose",
        "why",
        // copula / auxiliary verbs
        "am",
        "are",
        "be",
        "been",
        "being",
        "did",
        "do",
        "does",
        "done",
        "had",
        "has",
        "have",
        "is",
        "was",
        "were",
        "will",
        // modals
        "can",
        "could",
        "might",
        "must",
        "shall",
        "should",
        "would",
        // quantifiers / determiners
        "all",
        "any",
        "both",
        "each",
        "every",
        "few",
        "many",
        "more",
        "most",
        "much",
        "neither",
        "none",
        "some",
        "such",
        // adverbs / sentence-initial conjuncts capitalized only at sentence start
        "again",
        "also",
        "always",
        "ever",
        "finally",
        "first",
        "hence",
        "however",
        "instead",
        "just",
        "last",
        "meanwhile",
        "never",
        "next",
        "once",
        "often",
        "only",
        "otherwise",
        "sometimes",
        "still",
        "then",
        "therefore",
        "thus",
    ];
    COMMON_WORDS.contains(&token.to_ascii_lowercase().as_str())
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
    if normalized_tool.starts_with("tracedecay_") {
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

/// Returns true for `tracedecay_*` MCP tool names found in stored session
/// messages.
fn is_tracedecay_tool(token: &str) -> bool {
    let normalized = token.replace('-', "_").to_ascii_lowercase();
    normalized.starts_with("tracedecay_")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_verbs_match_across_inflections() {
        // Base forms preserved from the old exact-string list.
        for word in [
            "Add",
            "Avoid",
            "Create",
            "Delete",
            "Do",
            "Fix",
            "Implement",
            "Keep",
            "Persist",
            "Prefer",
            "Record",
            "Remove",
            "Update",
            "Use",
        ] {
            assert!(is_non_entity_leading_word(word), "{word} should match");
        }
        // Inflections that the old list missed (Risk A).
        for word in [
            "Adds",
            "Added",
            "Adding",
            "Prefers",
            "Preferred",
            "Preferring",
            "Uses",
            "Used",
            "Using",
            "Avoids",
            "Avoided",
            "Avoiding",
            "Creates",
            "Created",
            "Creating",
            "Deletes",
            "Deleted",
            "Deleting",
            "Does",
            "Did",
            "Done",
            "Doing",
            "Fixes",
            "Fixed",
            "Fixing",
            "Implements",
            "Implemented",
            "Implementing",
            "Keeps",
            "Kept",
            "Keeping",
            "Persists",
            "Persisted",
            "Persisting",
            "Records",
            "Recorded",
            "Recording",
            "Removes",
            "Removed",
            "Removing",
            "Updates",
            "Updated",
            "Updating",
        ] {
            assert!(
                is_non_entity_leading_word(word),
                "{word} should match an inflection"
            );
        }
        // Case-insensitive.
        assert!(is_non_entity_leading_word("PREFERS"));
        // Real proper nouns must not be treated as leading verbs.
        for word in [
            "Tokio",
            "Postgres",
            "Kubernetes",
            "Database",
            "Project",
            "Acme",
        ] {
            assert!(!is_non_entity_leading_word(word), "{word} should not match");
        }
    }

    #[test]
    fn single_capitalized_proper_nouns_are_extracted() {
        // Mid-sentence single tokens that the old >=2-word rule dropped.
        let entities = extract_entities("The backend standardized on Postgres in 2023");
        assert!(entities.contains(&"Postgres".to_string()));
        assert!(!entities.contains(&"The".to_string()));

        let entities = extract_entities("The deploy runs on Kubernetes with three replicas");
        assert!(entities.contains(&"Kubernetes".to_string()));
        assert!(!entities.contains(&"The".to_string()));

        // Sentence-initial content word is still captured — this is what makes
        // reason(["database"]) non-empty against the eval fixture (F7).
        let entities = extract_entities("Database backups run via pg_dump every night");
        assert!(entities.contains(&"Database".to_string()));
    }

    #[test]
    fn leading_verb_no_longer_swallows_following_entity() {
        // Risk A: "Prefers" was absent from the exact list, so "Prefers Tokio"
        // was captured verbatim and probe("Tokio") missed it.
        let entities = extract_entities("Prefers Tokio for async runtime");
        assert!(entities.contains(&"Tokio".to_string()));
        assert!(
            !entities.contains(&"Prefers Tokio".to_string()),
            "verb-led phrase must not be captured verbatim"
        );
        assert!(!entities.contains(&"Prefers".to_string()));
    }

    #[test]
    fn verb_led_multiword_phrase_exposes_head_noun() {
        let entities = extract_entities("Avoid Foo Bar when possible");
        assert!(
            entities.contains(&"Foo Bar".to_string()),
            "remainder phrase captured"
        );
        assert!(entities.contains(&"Bar".to_string()), "head noun exposed");
        assert!(!entities.contains(&"Avoid Foo Bar".to_string()));
        assert!(!entities.contains(&"Avoid".to_string()));
    }

    #[test]
    fn ordinary_multiword_phrase_keeps_phrase_only() {
        // Non-verb-led noun phrases keep the phrase as the entity and do not
        // also emit the bare head noun (avoids noisy single-word entities such
        // as Corp/Memory/Lens that would worsen retrieval Risk G).
        let entities = extract_entities("Acme Corp uses Postgres for its primary database");
        assert!(entities.contains(&"Acme Corp".to_string()));
        assert!(entities.contains(&"Postgres".to_string()));
        assert!(!entities.contains(&"Corp".to_string()));
    }

    #[test]
    fn lone_leading_verb_yields_nothing() {
        let entities = extract_entities("Use pnpm for installing dependencies");
        assert!(
            entities.is_empty(),
            "a lone leading verb with no capitalized entity yields no entities"
        );
    }

    #[test]
    fn sentence_initial_function_words_are_filtered() {
        let entities = extract_entities("Then we shipped it. Always back up the database.");
        assert!(!entities.contains(&"Then".to_string()));
        assert!(!entities.contains(&"Always".to_string()));
    }

    #[test]
    fn capitalized_sequence_keeps_phrase_and_skips_head_noun() {
        // Non-verb-led phrase keeps the phrase as the entity and does NOT emit
        // the bare head noun (Phoenix). The single proper noun "Rust" IS now
        // captured by the new single-token rule — that is intended coverage,
        // not a regression of the >=2-word phrase behavior.
        let entities = extract_entities("Project Phoenix ships fast and uses Rust");
        assert!(entities.contains(&"Project Phoenix".to_string()));
        assert!(!entities.contains(&"Phoenix".to_string()));
        assert!(entities.contains(&"Rust".to_string()));
    }
}
