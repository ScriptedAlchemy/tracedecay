//! Deterministic FHRR encodings for memory facts, entities, and queries.

use amari_holographic::{BindingAlgebra, FHRRAlgebra};
use sha2::{Digest, Sha256};

type Fhrr2048 = FHRRAlgebra<2048>;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HolographicEncodingError {
    #[error("holographic vector has dimension {actual}; expected {expected}")]
    DimensionMismatch { expected: usize, actual: usize },
}

#[derive(Clone, Debug, Default)]
pub struct HolographicEncoder;

impl HolographicEncoder {
    pub const DIMENSIONS: usize = 2048;
    pub const ROLE_CONTENT: &'static str = "__hrr_role_content__";
    pub const ROLE_ENTITY: &'static str = "__hrr_role_entity__";

    pub const fn new() -> Self {
        Self
    }

    pub(crate) fn encode_atom(&self, label: &str) -> Vec<f64> {
        normalize_coefficients(deterministic_coefficients(label))
    }

    pub fn encode_text(&self, text: &str) -> Result<Vec<f64>, HolographicEncodingError> {
        let tokens = tokenize_text(text);
        let Some((first, rest)) = tokens.split_first() else {
            return Ok(self.encode_atom("text:__hrr_empty__"));
        };
        average_coefficients(
            self.encode_atom(&format!("text:{first}")),
            rest.iter()
                .map(|token| self.encode_atom(&format!("text:{token}"))),
        )
    }

    pub fn encode_fact(
        &self,
        content: &str,
        entities: &[String],
    ) -> Result<Vec<f64>, HolographicEncodingError> {
        let content_role = to_fhrr(&self.encode_atom(Self::ROLE_CONTENT))?;
        let content_value = to_fhrr(&self.encode_text(content)?)?;
        let content_component = content_role.bind(&content_value).to_coefficients();
        let mut entity_components = Vec::new();

        let mut normalized_entities: Vec<String> = entities
            .iter()
            .map(|entity| entity.to_ascii_lowercase())
            .filter(|entity| !entity.trim().is_empty())
            .collect();
        normalized_entities.sort();
        normalized_entities.dedup();

        for entity in normalized_entities {
            let role = to_fhrr(&self.encode_atom(Self::ROLE_ENTITY))?;
            let value = to_fhrr(&self.encode_text(&entity)?)?;
            let bound = role.bind(&value);
            entity_components.push(bound.to_coefficients());
        }

        average_coefficients(content_component, entity_components)
    }

    pub fn similarity(&self, left: &[f64], right: &[f64]) -> Result<f64, HolographicEncodingError> {
        Ok(to_fhrr(left)?.similarity(&to_fhrr(right)?))
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

fn average_coefficients(
    first: Vec<f64>,
    rest: impl IntoIterator<Item = Vec<f64>>,
) -> Result<Vec<f64>, HolographicEncodingError> {
    if first.len() != HolographicEncoder::DIMENSIONS {
        return Err(HolographicEncodingError::DimensionMismatch {
            expected: HolographicEncoder::DIMENSIONS,
            actual: first.len(),
        });
    }
    let mut average = first;
    let mut count = 1.0;
    for vector in rest {
        if vector.len() != HolographicEncoder::DIMENSIONS {
            return Err(HolographicEncodingError::DimensionMismatch {
                expected: HolographicEncoder::DIMENSIONS,
                actual: vector.len(),
            });
        }
        count += 1.0;
        for (target, value) in average.iter_mut().zip(&vector) {
            *target += value;
        }
    }
    for value in &mut average {
        *value /= count;
    }
    Ok(normalize_coefficients(average))
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

fn to_fhrr(coefficients: &[f64]) -> Result<Fhrr2048, HolographicEncodingError> {
    if coefficients.len() != HolographicEncoder::DIMENSIONS {
        return Err(HolographicEncodingError::DimensionMismatch {
            expected: HolographicEncoder::DIMENSIONS,
            actual: coefficients.len(),
        });
    }
    Fhrr2048::from_coefficients(coefficients).map_err(|_| {
        HolographicEncodingError::DimensionMismatch {
            expected: HolographicEncoder::DIMENSIONS,
            actual: coefficients.len(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{HolographicEncoder, HolographicEncodingError};

    #[test]
    fn fact_encoding_is_restart_deterministic_without_persisted_vectors() {
        let entities = vec!["SQLite".to_owned(), "Grafeo".to_owned()];
        let first = HolographicEncoder::new()
            .encode_fact("canonical facts use deterministic FHRR", &entities)
            .unwrap();
        let reopened = HolographicEncoder::new()
            .encode_fact("canonical facts use deterministic FHRR", &entities)
            .unwrap();

        assert_eq!(first.len(), HolographicEncoder::DIMENSIONS);
        assert_eq!(first, reopened);
    }

    #[test]
    fn similarity_rejects_noncanonical_dimensions() {
        let error = HolographicEncoder::new()
            .similarity(&[0.0; 3], &[0.0; 3])
            .unwrap_err();

        assert_eq!(
            error,
            HolographicEncodingError::DimensionMismatch {
                expected: HolographicEncoder::DIMENSIONS,
                actual: 3,
            },
        );
    }
}
