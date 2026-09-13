use std::collections::BTreeSet;

use tracedecay_domain::RetrievalAnchorId;

/// Term count above which the drop-one relaxation tier is not planned.
///
/// The tier expands to one conjunction per dropped term, so its expression
/// grows as the square of the term count. Past this width the expansion is
/// broader than the strict scan it stands in for and states nothing a reader
/// would recognise as "the same query, one term short".
pub const LEXICAL_RELAXED_MAX_TERMS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CandidateChannel {
    Scope,
    Anchor,
    ExactMessage,
    Phrase,
    Entity,
    Time,
    /// Strict tier of the lexical ladder: every term must match one message.
    Lexical,
    /// The one relaxation tier: any all-but-one subset of the terms must match.
    ///
    /// Reached only after [`CandidateChannel::Lexical`] is verified empty under
    /// the same filters and snapshot, so a query answered strictly never pays
    /// for it.
    LexicalRelaxed,
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

    #[hotpath::skip]
    pub const fn has_semantic_channel(&self) -> bool {
        false
    }
}

#[hotpath::measure(label = "temporal.candidates.plan_scope")]
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

#[hotpath::measure(label = "temporal.candidates.plan_anchor")]
pub fn plan_anchor(anchor_id: &RetrievalAnchorId) -> CandidatePlan {
    CandidatePlan {
        clauses: vec![CandidateClause {
            channel: CandidateChannel::Anchor,
            value: anchor_id.to_string(),
            exact: true,
        }],
    }
}

#[hotpath::measure(label = "temporal.candidates.plan_text")]
pub fn plan_candidates(query: &str) -> CandidatePlan {
    let query = query.trim();
    if query.is_empty() {
        return CandidatePlan::default();
    }

    let (phrases, remainder) = split_quoted(query);
    let mut clauses = Vec::new();
    let mut seen = BTreeSet::new();
    let mut lexical_tokens: Vec<&str> = Vec::new();

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
        if token.is_empty() {
            continue;
        }
        if is_fts_operator(token) {
            // An uppercase boolean operator is structure the query typed, so it
            // survives into the lexical clause and keeps the semantics it asked
            // for. Every other spelling is a term-position stop word, and NEAR
            // needs a call shape no bare token supplies.
            if is_fts_boolean_operator(token) {
                lexical_tokens.push(token);
            }
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
        lexical_tokens.push(token);
    }
    push_lexical_ladder(&mut clauses, &mut seen, &lexical_tokens);

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

/// Pushes the strict lexical clause and, where one exists, its single
/// relaxation tier.
///
/// A single term has no weaker conjunction to fall back to, a typed boolean
/// expression already says what to match, and past [`LEXICAL_RELAXED_MAX_TERMS`]
/// the drop-one expansion is broader than the strict scan it stands in for.
fn push_lexical_ladder(
    clauses: &mut Vec<CandidateClause>,
    seen: &mut BTreeSet<(CandidateChannel, String)>,
    tokens: &[&str],
) {
    let lexical = normalize_lexical_tokens(tokens);
    if lexical.tokens.is_empty() {
        return;
    }
    let value = lexical.tokens.join(" ");
    push_clause(clauses, seen, CandidateChannel::Lexical, value.clone());
    if !lexical.has_boolean_operator
        && (2..=LEXICAL_RELAXED_MAX_TERMS).contains(&lexical.tokens.len())
    {
        push_clause(clauses, seen, CandidateChannel::LexicalRelaxed, value);
    }
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

/// Boolean operators a lexical clause can carry between two terms.
///
/// Case matters the way it matters to FTS5: `or` is a word people search for,
/// `OR` is the operator they typed. `NEAR` is excluded because it is a call,
/// not an infix, and a bare token cannot supply its arguments.
pub fn is_fts_boolean_operator(token: &str) -> bool {
    matches!(token, "AND" | "OR" | "NOT")
}

struct LexicalTokens<'a> {
    tokens: Vec<&'a str>,
    has_boolean_operator: bool,
}

/// Drops the operator positions no boolean expression can use — leading,
/// trailing, and repeated — so the clause the store receives always alternates
/// term, operator, term. Terms are deduplicated only for a plain conjunction,
/// where a repeat adds no constraint; rewriting a typed expression would change
/// what it asked for.
fn normalize_lexical_tokens<'a>(tokens: &[&'a str]) -> LexicalTokens<'a> {
    let mut kept: Vec<&'a str> = Vec::with_capacity(tokens.len());
    let mut has_boolean_operator = false;
    for token in tokens {
        if is_fts_boolean_operator(token) {
            if kept
                .last()
                .is_none_or(|previous| is_fts_boolean_operator(previous))
            {
                continue;
            }
            has_boolean_operator = true;
            kept.push(token);
            continue;
        }
        kept.push(token);
    }
    if kept
        .last()
        .is_some_and(|last| is_fts_boolean_operator(last))
    {
        kept.pop();
        has_boolean_operator = kept.iter().copied().any(is_fts_boolean_operator);
    }
    if !has_boolean_operator {
        let mut seen = BTreeSet::new();
        kept.retain(|token| seen.insert(*token));
    }
    LexicalTokens {
        tokens: kept,
        has_boolean_operator,
    }
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
        assert!(plan.contains(
            CandidateChannel::Lexical,
            r#"panic!("boom") E0425 日本語 🚨 foo::bar"#
        ));
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
    fn lexical_terms_share_one_deduplicated_candidate_lane() {
        let plan = plan_candidates("workflow correction workflow repeated");
        let lexical = plan
            .clauses()
            .iter()
            .filter(|clause| clause.channel == CandidateChannel::Lexical)
            .collect::<Vec<_>>();

        assert_eq!(lexical.len(), 1);
        assert_eq!(lexical[0].value, "workflow correction repeated");
    }

    #[test]
    fn plain_multi_term_queries_plan_one_relaxation_tier() {
        let plan = plan_candidates("workflow correction repeated failure");

        assert!(plan.contains(
            CandidateChannel::Lexical,
            "workflow correction repeated failure"
        ));
        assert!(plan.contains(
            CandidateChannel::LexicalRelaxed,
            "workflow correction repeated failure"
        ));
    }

    #[test]
    fn single_terms_and_typed_boolean_queries_plan_no_relaxation_tier() {
        for query in ["workflow", "workflow OR correction", "workflow NOT stale"] {
            let plan = plan_candidates(query);
            assert!(
                !plan
                    .clauses()
                    .iter()
                    .any(|clause| clause.channel == CandidateChannel::LexicalRelaxed),
                "{query} planned a relaxation tier"
            );
        }

        let wide = (0..=LEXICAL_RELAXED_MAX_TERMS)
            .map(|index| format!("term{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            !plan_candidates(&wide)
                .clauses()
                .iter()
                .any(|clause| clause.channel == CandidateChannel::LexicalRelaxed)
        );
    }

    #[test]
    fn typed_boolean_operators_survive_into_the_lexical_clause() {
        assert!(
            plan_candidates("alpha OR beta").contains(CandidateChannel::Lexical, "alpha OR beta")
        );
        assert!(
            plan_candidates("alpha and beta").contains(CandidateChannel::Lexical, "alpha beta"),
            "lowercase and is a term-position word, not an operator"
        );
        // Operator positions no expression can use are dropped, never emitted
        // into a clause the store would fail to parse.
        assert!(plan_candidates("OR alpha").contains(CandidateChannel::Lexical, "alpha"));
        assert!(plan_candidates("alpha OR").contains(CandidateChannel::Lexical, "alpha"));
        assert!(
            plan_candidates("alpha OR AND beta")
                .contains(CandidateChannel::Lexical, "alpha OR beta")
        );
        assert!(
            plan_candidates("alpha NEAR beta").contains(CandidateChannel::Lexical, "alpha beta")
        );
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
        assert!(plan.contains(CandidateChannel::Lexical, query));
        assert!(plan.contains(CandidateChannel::Entity, "panic!(\"x\")"));
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
        assert!(plan.contains(CandidateChannel::Lexical, r#"prefix"not-a-phrase suffix"#));

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
}
