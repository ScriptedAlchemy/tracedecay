//! Deterministic FHRR encodings for memory facts, entities, and queries.

use amari_holographic::{BindingAlgebra, FHRRAlgebra};
use sha2::{Digest, Sha256};

type Fhrr2048 = FHRRAlgebra<2048>;

#[derive(Clone, Debug, Default)]
pub struct HolographicEncoder;

impl HolographicEncoder {
    pub const DIMENSIONS: usize = 2048;
    pub const ROLE_CONTENT: &'static str = "__hrr_role_content__";
    pub const ROLE_ENTITY: &'static str = "__hrr_role_entity__";

    pub const fn new() -> Self {
        Self
    }

    pub fn encode_atom(&self, label: &str) -> Vec<f64> {
        normalize_coefficients(deterministic_coefficients(label))
    }

    pub fn encode_text(&self, text: &str) -> Vec<f64> {
        let tokens = tokenize_text(text);
        if tokens.is_empty() {
            return self.encode_atom("text:__hrr_empty__");
        }
        let vectors: Vec<Vec<f64>> = tokens
            .iter()
            .map(|token| self.encode_atom(&format!("text:{token}")))
            .collect();
        average_coefficients(&vectors)
    }

    pub fn encode_fact(&self, content: &str, entities: &[String]) -> Vec<f64> {
        let (Some(content_role), Some(content_value)) = (
            to_fhrr(&self.encode_atom(Self::ROLE_CONTENT)),
            to_fhrr(&self.encode_text(content)),
        ) else {
            return Vec::new();
        };
        let mut components = vec![content_role.bind(&content_value).to_coefficients()];

        let mut normalized_entities: Vec<String> = entities
            .iter()
            .map(|entity| entity.to_ascii_lowercase())
            .filter(|entity| !entity.trim().is_empty())
            .collect();
        normalized_entities.sort();
        normalized_entities.dedup();

        for entity in normalized_entities {
            let (Some(role), Some(value)) = (
                to_fhrr(&self.encode_atom(Self::ROLE_ENTITY)),
                to_fhrr(&self.encode_text(&entity)),
            ) else {
                continue;
            };

            let bound = role.bind(&value);
            components.push(bound.to_coefficients());
        }

        average_coefficients(&components)
    }

    pub fn similarity(&self, left: &[f64], right: &[f64]) -> f64 {
        if let (Some(left_fhrr), Some(right_fhrr)) = (to_fhrr(left), to_fhrr(right)) {
            return left_fhrr.similarity(&right_fhrr);
        }

        cosine_similarity(left, right)
    }

    pub fn serialize(coefficients: &[f64]) -> bincode::Result<Vec<u8>> {
        bincode::serialize(&coefficients.to_vec())
    }

    pub fn deserialize(bytes: &[u8]) -> bincode::Result<Vec<f64>> {
        bincode::deserialize(bytes)
    }
}

fn deterministic_coefficients(label: &str) -> Vec<f64> {
    let mut coefficients = Vec::with_capacity(HolographicEncoder::DIMENSIONS);
    let mut counter = 0_u64;

    while coefficients.len() < HolographicEncoder::DIMENSIONS {
        let mut hasher = Sha256::new();
        hasher.update(label.as_bytes());
        hasher.update(counter.to_le_bytes());
        let digest = hasher.finalize();

        for chunk in digest.chunks_exact(8) {
            if coefficients.len() == HolographicEncoder::DIMENSIONS {
                break;
            }

            let mut bytes = [0_u8; 8];
            bytes.copy_from_slice(chunk);
            let unit = u64::from_le_bytes(bytes) as f64 / u64::MAX as f64;
            coefficients.push(unit.mul_add(2.0, -1.0));
        }

        counter = counter.saturating_add(1);
    }

    coefficients
}

fn tokenize_text(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || matches!(ch, '_' | '/' | ':' | '.') {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            push_token(&mut tokens, &mut current);
        }
    }
    if !current.is_empty() {
        push_token(&mut tokens, &mut current);
    }
    tokens.sort();
    tokens.dedup();
    tokens
}

fn push_token(tokens: &mut Vec<String>, current: &mut String) {
    if current.len() >= 2 {
        tokens.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

fn average_coefficients(vectors: &[Vec<f64>]) -> Vec<f64> {
    if vectors.is_empty() {
        return vec![0.0; HolographicEncoder::DIMENSIONS];
    }
    let mut average = vec![0.0; HolographicEncoder::DIMENSIONS];
    let mut count = 0.0;
    for vector in vectors {
        if vector.len() != HolographicEncoder::DIMENSIONS {
            continue;
        }
        count += 1.0;
        for (target, value) in average.iter_mut().zip(vector) {
            *target += value;
        }
    }
    if count > 0.0 {
        for value in &mut average {
            *value /= count;
        }
    }
    normalize_coefficients(average)
}

fn normalize_coefficients(mut coefficients: Vec<f64>) -> Vec<f64> {
    let norm = coefficients
        .iter()
        .map(|coefficient| coefficient * coefficient)
        .sum::<f64>()
        .sqrt();

    if norm > f64::EPSILON {
        for coefficient in &mut coefficients {
            *coefficient /= norm;
        }
    }

    coefficients
}

fn to_fhrr(coefficients: &[f64]) -> Option<Fhrr2048> {
    Fhrr2048::from_coefficients(coefficients).ok()
}

fn cosine_similarity(left: &[f64], right: &[f64]) -> f64 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }

    let (dot, left_norm, right_norm) = left
        .iter()
        .zip(right.iter())
        .fold((0.0, 0.0, 0.0), |(dot, left_norm, right_norm), (l, r)| {
            (dot + l * r, left_norm + l * l, right_norm + r * r)
        });

    if left_norm <= f64::EPSILON || right_norm <= f64::EPSILON {
        0.0
    } else {
        dot / (left_norm.sqrt() * right_norm.sqrt())
    }
}
