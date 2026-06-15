//! Content-similarity primitives shared by the write-time diff check and the
//! dashboard's similarity/curation analytics.
//!
//! Moved here from `src/dashboard/memory_analysis.rs` so `MemoryStore::add_fact`
//! can classify near-duplicates at write time without depending on the
//! dashboard; the dashboard re-exports these to keep its behavior identical.

use std::collections::BTreeSet;

const TOKEN_STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "have", "in", "is",
    "it", "of", "on", "or", "that", "the", "this", "to", "was", "were", "with",
];

pub fn content_tokens(content: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut current = String::new();
    for ch in content.chars() {
        let lower = ch.to_ascii_lowercase();
        let is_token_char = if current.is_empty() {
            lower.is_ascii_alphanumeric()
        } else {
            lower.is_ascii_alphanumeric() || lower == '_' || lower == '\'' || lower == '-'
        };
        if is_token_char {
            current.push(lower);
        } else if !current.is_empty() {
            tokens.insert(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.insert(current);
    }
    tokens
        .into_iter()
        .map(|t| {
            t.trim_matches(|c| c == '_' || c == '\'' || c == '-')
                .to_string()
        })
        .filter(|t| t.len() > 1 && !TOKEN_STOPWORDS.contains(&t.as_str()))
        .collect()
}

pub fn lexical_overlap(a: &str, b: &str) -> (serde_json::Value, f64, f64) {
    let a_tokens = content_tokens(a);
    let b_tokens = content_tokens(b);
    let shared: Vec<&String> = a_tokens.intersection(&b_tokens).collect();
    let union = a_tokens.union(&b_tokens).count();
    let min_size = a_tokens.len().min(b_tokens.len());
    let token_overlap = if union > 0 {
        (shared.len() as f64 / union as f64 * 10_000.0).round() / 10_000.0
    } else {
        0.0
    };
    let overlap_coefficient = if min_size > 0 {
        (shared.len() as f64 / min_size as f64 * 10_000.0).round() / 10_000.0
    } else {
        0.0
    };
    let payload = serde_json::json!({
        "token_overlap": token_overlap,
        "overlap_coefficient": overlap_coefficient,
        "shared_token_count": shared.len(),
        "a_token_count": a_tokens.len(),
        "b_token_count": b_tokens.len(),
        "shared_tokens": shared.iter().take(10).collect::<Vec<_>>(),
    });
    (payload, token_overlap, overlap_coefficient)
}

pub fn similarity_classification(
    similarity: f64,
    token_overlap: f64,
    overlap_coefficient: f64,
) -> &'static str {
    if similarity >= 0.95 && (overlap_coefficient >= 0.65 || token_overlap >= 0.45) {
        return "likely_duplicate";
    }
    if similarity >= 0.90 && (overlap_coefficient >= 0.35 || token_overlap >= 0.20) {
        return "merge_candidate";
    }
    if similarity >= 0.97 && overlap_coefficient >= 0.25 {
        return "merge_candidate";
    }
    "related"
}

/// Phase-cosine similarity between two equal-length phase vectors.
pub fn phase_cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| (x - y).cos()).sum::<f64>() / a.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_tokens_strips_stopwords() {
        let tokens = content_tokens("The quick brown fox and the lazy dog");
        assert!(tokens.contains("quick"));
        assert!(tokens.contains("brown"));
        assert!(!tokens.contains("the"));
        assert!(!tokens.contains("and"));
    }

    #[test]
    fn lexical_overlap_identical_text() {
        let text = "hello world example";
        let (payload, token_overlap, overlap_coefficient) = lexical_overlap(text, text);
        assert!((token_overlap - 1.0).abs() < f64::EPSILON);
        assert!((overlap_coefficient - 1.0).abs() < f64::EPSILON);
        assert_eq!(payload["shared_token_count"], 3);
    }

    #[test]
    fn similarity_classification_tiers() {
        assert_eq!(
            similarity_classification(0.96, 0.5, 0.7),
            "likely_duplicate"
        );
        assert_eq!(
            similarity_classification(0.92, 0.25, 0.4),
            "merge_candidate"
        );
        assert_eq!(similarity_classification(0.80, 0.1, 0.1), "related");
    }

    #[test]
    fn phase_cosine_identical_vectors() {
        let v = vec![0.1, 0.2, 0.3];
        let sim = phase_cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-10);
    }
}
