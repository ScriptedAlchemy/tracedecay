use std::collections::BTreeSet;

use tracedecay_domain::RetrievalAnchorId;

#[cfg(test)]
use tracedecay_domain::ByteRangeV1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CandidateChannel {
    Scope,
    Anchor,
    ExactMessage,
    Phrase,
    Entity,
    Time,
    Lexical,
    Summary,
    Span,
    Burst,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateClause {
    pub channel: CandidateChannel,
    pub value: String,
    pub exact: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CandidatePlan {
    clauses: Vec<CandidateClause>,
}

impl CandidatePlan {
    pub fn clauses(&self) -> &[CandidateClause] {
        &self.clauses
    }

    pub fn contains(&self, channel: CandidateChannel, value: &str) -> bool {
        self.clauses
            .iter()
            .any(|clause| clause.channel == channel && clause.value == value)
    }

    pub const fn has_semantic_channel(&self) -> bool {
        false
    }
}

pub fn plan_scope_candidates() -> CandidatePlan {
    CandidatePlan {
        clauses: vec![
            CandidateClause {
                channel: CandidateChannel::Scope,
                value: String::new(),
                exact: false,
            },
            // A scope browse also lists the scope's published summary nodes
            // (an empty Summary clause is a listing, not a text match);
            // without it an empty-query page is summary-blind while the
            // participant's summary frontier says otherwise.
            CandidateClause {
                channel: CandidateChannel::Summary,
                value: String::new(),
                exact: false,
            },
        ],
    }
}

pub fn plan_anchor(anchor_id: &RetrievalAnchorId) -> CandidatePlan {
    CandidatePlan {
        clauses: vec![CandidateClause {
            channel: CandidateChannel::Anchor,
            value: anchor_id.to_string(),
            exact: true,
        }],
    }
}

pub fn plan_candidates(query: &str) -> CandidatePlan {
    let query = query.trim();
    if query.is_empty() {
        return CandidatePlan::default();
    }

    let (phrases, remainder) = split_quoted(query);
    let mut clauses = Vec::new();
    let mut seen = BTreeSet::new();

    push_clause(
        &mut clauses,
        &mut seen,
        CandidateChannel::ExactMessage,
        query.to_string(),
    );

    for phrase in phrases {
        push_clause(&mut clauses, &mut seen, CandidateChannel::Phrase, phrase);
    }

    if looks_like_command(query) {
        push_clause(
            &mut clauses,
            &mut seen,
            CandidateChannel::Entity,
            query.to_string(),
        );
    }

    for token in remainder.split_whitespace() {
        if token.is_empty() || is_fts_operator(token) {
            continue;
        }
        if looks_like_iso_date(token) {
            push_clause(
                &mut clauses,
                &mut seen,
                CandidateChannel::Time,
                token.to_string(),
            );
        }
        if looks_like_exact_entity(token) {
            push_clause(
                &mut clauses,
                &mut seen,
                CandidateChannel::Entity,
                token.to_string(),
            );
        }
        push_clause(
            &mut clauses,
            &mut seen,
            CandidateChannel::Lexical,
            token.to_string(),
        );
    }

    push_clause(
        &mut clauses,
        &mut seen,
        CandidateChannel::Summary,
        query.to_string(),
    );
    push_clause(
        &mut clauses,
        &mut seen,
        CandidateChannel::Span,
        query.to_string(),
    );
    push_clause(
        &mut clauses,
        &mut seen,
        CandidateChannel::Burst,
        query.to_string(),
    );
    CandidatePlan { clauses }
}

fn push_clause(
    clauses: &mut Vec<CandidateClause>,
    seen: &mut BTreeSet<(CandidateChannel, String)>,
    channel: CandidateChannel,
    value: String,
) {
    if value.is_empty() || !seen.insert((channel, value.clone())) {
        return;
    }
    let exact = matches!(
        channel,
        CandidateChannel::ExactMessage
            | CandidateChannel::Phrase
            | CandidateChannel::Entity
            | CandidateChannel::Time
            | CandidateChannel::Span
            | CandidateChannel::Burst
    );
    clauses.push(CandidateClause {
        channel,
        value,
        exact,
    });
}

fn split_quoted(text: &str) -> (Vec<String>, String) {
    let mut phrases = Vec::new();
    let mut remainder = String::with_capacity(text.len());
    let mut in_quote = false;
    let mut current = String::new();
    let mut at_token_boundary = true;
    let mut chars = text.chars().peekable();

    while let Some(character) = chars.next() {
        if in_quote {
            if character == '\\' {
                if let Some('"' | '\\') = chars.peek().copied() {
                    if let Some(escaped) = chars.next() {
                        current.push(escaped);
                        remainder.push(' ');
                    }
                } else {
                    current.push(character);
                    remainder.push(' ');
                }
                at_token_boundary = false;
                continue;
            }
            if character == '"' {
                let phrase = current.trim();
                if !phrase.is_empty() {
                    phrases.push(phrase.to_string());
                }
                current.clear();
                in_quote = false;
                remainder.push(' ');
                at_token_boundary = true;
                continue;
            }
            current.push(character);
            remainder.push(' ');
            at_token_boundary = character.is_whitespace();
            continue;
        }

        if character == '"' && at_token_boundary {
            in_quote = true;
            remainder.push(' ');
            at_token_boundary = false;
            continue;
        }

        remainder.push(character);
        at_token_boundary = character.is_whitespace();
    }

    if in_quote {
        let unmatched = current.trim();
        if !unmatched.is_empty() {
            remainder.push_str(unmatched);
        }
    }
    (phrases, remainder)
}

fn is_fts_operator(token: &str) -> bool {
    matches!(
        token.to_ascii_uppercase().as_str(),
        "AND" | "OR" | "NOT" | "NEAR"
    )
}

fn looks_like_iso_date(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

fn looks_like_exact_entity(token: &str) -> bool {
    token.contains('/')
        || token.contains('\\')
        || token.contains("::")
        || token.contains("!(")
        || token.starts_with("--")
        || token.starts_with('$')
        || looks_like_rust_error_code(token)
}

fn looks_like_rust_error_code(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.len() == 5 && bytes[0] == b'E' && bytes[1..].iter().all(u8::is_ascii_digit)
}

fn looks_like_command(query: &str) -> bool {
    let first = query.split_whitespace().next().unwrap_or_default();
    matches!(
        first,
        "cargo"
            | "git"
            | "rg"
            | "grep"
            | "tracedecay"
            | "npm"
            | "pnpm"
            | "yarn"
            | "python"
            | "python3"
            | "node"
            | "bash"
            | "sh"
    ) || first.starts_with('$')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planning_preserves_quoted_punctuation_path_error_cjk_and_emoji_exactness() {
        let plan = plan_candidates(
            r#""fatal: path/to/file.rs:42" panic!("boom") E0425 日本語 🚨 foo::bar"#,
        );

        assert!(plan.contains(CandidateChannel::Phrase, "fatal: path/to/file.rs:42"));
        assert!(plan.contains(CandidateChannel::Entity, "panic!(\"boom\")"));
        assert!(plan.contains(CandidateChannel::Entity, "E0425"));
        assert!(plan.contains(CandidateChannel::Lexical, "日本語"));
        assert!(plan.contains(CandidateChannel::Lexical, "🚨"));
        assert!(plan.contains(CandidateChannel::Entity, "foo::bar"));
        assert!(!plan.has_semantic_channel());
    }

    #[test]
    fn planning_preserves_exact_commands_and_dates() {
        let query = "cargo test --lib query::temporal::* 2026-07-18";
        let plan = plan_candidates(query);

        assert!(plan.contains(CandidateChannel::ExactMessage, query));
        assert!(plan.contains(CandidateChannel::Entity, query));
        assert!(plan.contains(CandidateChannel::Time, "2026-07-18"));
        assert!(plan.contains(CandidateChannel::Summary, query));
    }

    #[test]
    fn empty_queries_produce_no_candidates() {
        assert!(plan_candidates(" \t\n").clauses().is_empty());
    }

    #[test]
    fn direct_anchor_plan_is_exact_and_singleton() {
        let anchor = RetrievalAnchorId::new("anchor.direct").expect("anchor");
        let plan = plan_anchor(&anchor);

        assert_eq!(plan.clauses().len(), 1);
        assert!(plan.contains(CandidateChannel::Anchor, "anchor.direct"));
        assert!(plan.clauses()[0].exact);
    }

    #[test]
    fn split_quoted_parses_escaped_quotes_and_escaped_backslashes() {
        let plan = plan_candidates(r#""say \"hello\" world" trailing"#);
        assert!(plan.contains(CandidateChannel::Phrase, r#"say "hello" world"#));
        assert!(plan.contains(CandidateChannel::Lexical, "trailing"));

        let plan = plan_candidates(r#""path\\to\\file" kept"#);
        assert!(plan.contains(CandidateChannel::Phrase, r"path\to\file"));
        assert!(plan.contains(CandidateChannel::Lexical, "kept"));
    }

    #[test]
    fn planning_preserves_apostrophes_brackets_braces_commas_and_semicolons() {
        let query = "don't use [path/to/file.rs], {cfg:debug}; done";
        let plan = plan_candidates(query);

        assert!(plan.contains(CandidateChannel::ExactMessage, query));
        assert!(plan.contains(CandidateChannel::Lexical, "don't"));
        assert!(plan.contains(CandidateChannel::Entity, "[path/to/file.rs],"));
        assert!(plan.contains(CandidateChannel::Lexical, "{cfg:debug};"));
        assert!(plan.contains(CandidateChannel::Lexical, "done"));
    }

    #[test]
    fn punctuation_heavy_paths_errors_commands_cjk_emoji_stay_on_exact_message() {
        let query = r#"cargo check path/to/weird,file.rs; E0425 don't panic!("x") 日本語 🚨"#;
        let plan = plan_candidates(query);

        assert!(plan.contains(CandidateChannel::ExactMessage, query));
        assert!(
            plan.clauses()
                .iter()
                .any(|clause| clause.channel == CandidateChannel::ExactMessage && clause.exact)
        );
        assert!(plan.contains(CandidateChannel::Entity, query));
        assert!(plan.contains(CandidateChannel::Entity, "path/to/weird,file.rs;"));
        assert!(plan.contains(CandidateChannel::Entity, "E0425"));
        assert!(plan.contains(CandidateChannel::Lexical, "don't"));
        assert!(plan.contains(CandidateChannel::Entity, "panic!(\"x\")"));
        assert!(plan.contains(CandidateChannel::Lexical, "日本語"));
        assert!(plan.contains(CandidateChannel::Lexical, "🚨"));
        assert!(!plan.clauses().iter().any(|clause| clause.channel
            == CandidateChannel::ExactMessage
            && clause.value != query));
    }

    #[test]
    fn unmatched_and_mid_token_quotes_do_not_invent_phrases() {
        let plan = plan_candidates(r#"prefix"not-a-phrase suffix"#);
        assert!(
            !plan
                .clauses()
                .iter()
                .any(|clause| clause.channel == CandidateChannel::Phrase)
        );
        assert!(plan.contains(CandidateChannel::Lexical, r#"prefix"not-a-phrase"#));
        assert!(plan.contains(CandidateChannel::Lexical, "suffix"));

        let plan = plan_candidates(r#""unterminated phrase value"#);
        assert!(
            !plan
                .clauses()
                .iter()
                .any(|clause| clause.channel == CandidateChannel::Phrase)
        );
        assert!(plan.contains(
            CandidateChannel::ExactMessage,
            r#""unterminated phrase value"#
        ));
    }

    #[test]
    fn exact_match_byte_ranges_are_non_empty_and_half_open() {
        let range = ByteRangeV1::new(7, 11).expect("valid range");
        assert_eq!((range.start(), range.end()), (7, 11));
        assert!(ByteRangeV1::new(7, 7).is_err());
        assert!(ByteRangeV1::new(8, 7).is_err());
    }
}
