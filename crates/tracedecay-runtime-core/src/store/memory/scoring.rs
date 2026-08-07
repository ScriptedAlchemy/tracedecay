//! Compatibility scoring primitives. Kept byte-identical to `src/memory/retrieval.rs` formulas pending unification.

use std::collections::BTreeSet;

use crate::memory::encoding::HolographicEncoder;

use tracedecay_domain::UtcMicros;
use tracedecay_store::ProjectMemoryFactV1;

pub(super) fn project_memory_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '/' | ':' | '.') {
            current.push(character.to_ascii_lowercase());
        } else if !current.is_empty() {
            if current.len() >= 2 {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.len() >= 2 {
        tokens.push(current);
    }
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

pub(super) fn project_memory_fact_tokens(fact: &ProjectMemoryFactV1) -> Vec<String> {
    let mut tokens = fact
        .content()
        .map(project_memory_tokens)
        .unwrap_or_default();
    if let Some(tags) = fact.tags() {
        for tag in tags {
            tokens.extend(project_memory_tokens(tag));
        }
    }
    if let Some(entities) = fact.entities() {
        for entity in entities {
            tokens.extend(project_memory_tokens(entity));
        }
    }
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

pub(super) fn project_memory_term_coverage(query: &[String], fact: &[String]) -> f64 {
    if query.is_empty() {
        return 0.0;
    }
    let matched = query
        .iter()
        .filter(|query_token| {
            fact.iter().any(|fact_token| {
                fact_token == *query_token
                    || (query_token.len() >= 4 && fact_token.starts_with(query_token.as_str()))
            })
        })
        .count();
    matched as f64 / query.len() as f64
}

pub(super) fn project_memory_jaccard(left: &[String], right: &[String]) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let left = left.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let right = right.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let union = left.union(&right).count();
    if union == 0 {
        0.0
    } else {
        left.intersection(&right).count() as f64 / union as f64
    }
}

pub(super) fn project_memory_holographic_score(
    encoder: &HolographicEncoder,
    query_vector: &[f64],
    fact: &ProjectMemoryFactV1,
) -> f64 {
    let Some(content) = fact.content() else {
        return 0.0;
    };
    let fact_vector = encoder.encode_fact(content, fact.entities().unwrap_or_default());
    f64::midpoint(encoder.similarity(query_vector, &fact_vector), 1.0).clamp(0.0, 1.0)
}

pub(super) fn project_memory_millionths(value: f64) -> u32 {
    (value.clamp(0.0, 1.0) * 1_000_000.0).round() as u32
}

pub(super) fn project_memory_temporal_decay(updated_at: UtcMicros, now: UtcMicros) -> f64 {
    let age_micros = now.0.saturating_sub(updated_at.0).max(0) as f64;
    let age_days = age_micros / 86_400_000_000.0;
    0.5_f64.powf(age_days / 365.0).clamp(0.10, 1.0)
}
