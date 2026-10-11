//! Deterministic FHRR encodings for memory facts, entities, and queries.
//!
//! Vectors hold only the real FHRR coefficients; every imaginary part is zero,
//! so binding is the componentwise product and similarity is the cosine.

use std::sync::LazyLock;

use sha2::{Digest, Sha256};

/// The role atoms are compile-time constants, but deriving each one costs 256
/// SHA-256 blocks plus normalization. Both are computed once and reused across
/// every fact encoding.
static ROLE_CONTENT: LazyLock<Vec<f64>> =
    LazyLock::new(|| HolographicEncoder::new().encode_atom(HolographicEncoder::ROLE_CONTENT));
static ROLE_ENTITY: LazyLock<Vec<f64>> =
    LazyLock::new(|| HolographicEncoder::new().encode_atom(HolographicEncoder::ROLE_ENTITY));

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HolographicEncodingError {
    #[error("holographic vector has dimension {actual}; expected {expected}")]
    DimensionMismatch { expected: usize, actual: usize },
}

#[derive(Clone, Debug, Default)]
pub struct HolographicEncoder;

/// A dimension-checked query vector with its norm computed once.
///
/// Search scoring compares one query against every candidate fact; preparing
/// the query once leaves only the fact-side norm and the dot product per
/// candidate.
pub struct HolographicQueryVector {
    coefficients: Vec<f64>,
    norm: f64,
}

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
        let content_component = bind(&ROLE_CONTENT, &self.encode_text(content)?);
        let mut entity_components = Vec::new();

        let mut normalized_entities: Vec<String> = entities
            .iter()
            .map(|entity| entity.to_ascii_lowercase())
            .filter(|entity| !entity.trim().is_empty())
            .collect();
        normalized_entities.sort();
        normalized_entities.dedup();

        for entity in normalized_entities {
            entity_components.push(bind(&ROLE_ENTITY, &self.encode_text(&entity)?));
        }

        average_coefficients(content_component, entity_components)
    }

    pub fn similarity(&self, left: &[f64], right: &[f64]) -> Result<f64, HolographicEncodingError> {
        let query = self.prepare_query(left)?;
        self.query_similarity(&query, right)
    }

    pub fn prepare_query(
        &self,
        coefficients: &[f64],
    ) -> Result<HolographicQueryVector, HolographicEncodingError> {
        ensure_dimensions(coefficients)?;
        Ok(HolographicQueryVector {
            coefficients: coefficients.to_vec(),
            norm: norm(coefficients),
        })
    }

    pub fn query_similarity(
        &self,
        query: &HolographicQueryVector,
        fact_coefficients: &[f64],
    ) -> Result<f64, HolographicEncodingError> {
        ensure_dimensions(fact_coefficients)?;
        let fact_norm = norm(fact_coefficients);
        if query.norm < SIMILARITY_NORM_FLOOR || fact_norm < SIMILARITY_NORM_FLOOR {
            return Ok(0.0);
        }
        // Positive-zero start: `Iterator::sum` starts at negative zero, which
        // would flip the sign of a dot product made only of negative zeros.
        let dot = query
            .coefficients
            .iter()
            .zip(fact_coefficients)
            .fold(0.0, |sum, (left, right)| sum + left * right);
        Ok(dot / (query.norm * fact_norm))
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

        for chunk in digest.as_chunks::<8>().0 {
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
    ensure_dimensions(&first)?;
    let mut average = first;
    let mut count = 1.0;
    for vector in rest {
        ensure_dimensions(&vector)?;
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

/// Vectors below this norm compare as orthogonal to everything.
const SIMILARITY_NORM_FLOOR: f64 = 1e-10;

/// FHRR binding of real phasors: the componentwise product.
fn bind(role: &[f64], value: &[f64]) -> Vec<f64> {
    role.iter()
        .zip(value)
        .map(|(role, value)| role * value)
        .collect()
}

fn norm(coefficients: &[f64]) -> f64 {
    coefficients
        .iter()
        .fold(0.0, |sum, coefficient| sum + coefficient * coefficient)
        .sqrt()
}

fn ensure_dimensions(coefficients: &[f64]) -> Result<(), HolographicEncodingError> {
    if coefficients.len() == HolographicEncoder::DIMENSIONS {
        Ok(())
    } else {
        Err(HolographicEncodingError::DimensionMismatch {
            expected: HolographicEncoder::DIMENSIONS,
            actual: coefficients.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

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

    fn bits_digest(values: &[f64]) -> String {
        let mut hasher = Sha256::new();
        for value in values {
            hasher.update(value.to_bits().to_le_bytes());
        }
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn scaled(vector: &[f64], factor: f64) -> Vec<f64> {
        vector.iter().map(|value| value * factor).collect()
    }

    /// Bit-level outputs recorded from the amari FHRR implementation these
    /// real-coefficient operations replaced. Any drift in coefficients,
    /// signed zeros, or similarity rounding changes a digest.
    #[test]
    fn encoder_outputs_match_recorded_fhrr_bits() {
        let encoder = HolographicEncoder::new();
        let owned = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect();
        let entity_cases: [(&str, Vec<String>); 5] = [
            ("", Vec::new()),
            ("a b", vec![" ".to_owned()]),
            (
                "duplicate duplicate words",
                owned(&["SQLite", "sqlite", "Grafeo"]),
            ),
            ("Grüße 東京 naïve café", owned(&["Ünïcode", "東京"])),
            (
                "src/memory/encoding.rs crate::memory::encoding v1.2",
                owned(&["path/to/file.rs", "crate::memory"]),
            ),
        ];
        let facts: Vec<Vec<f64>> = entity_cases
            .iter()
            .map(|(content, entities)| encoder.encode_fact(content, entities).unwrap())
            .collect();
        let texts: Vec<Vec<f64>> = entity_cases
            .iter()
            .map(|(content, _)| encoder.encode_text(content).unwrap())
            .collect();

        let mut nonfinite = facts[2].clone();
        nonfinite[7] = f64::NAN;
        let mut infinite = facts[3].clone();
        infinite[11] = f64::INFINITY;
        let mut signed_zero = vec![0.0; HolographicEncoder::DIMENSIONS];
        signed_zero[0] = -0.0;
        signed_zero[1] = 1.0;
        let alternating = |even: f64, odd: f64| -> Vec<f64> {
            (0..HolographicEncoder::DIMENSIONS)
                .map(|index| if index % 2 == 0 { even } else { odd })
                .collect()
        };
        let even_positive = alternating(1.0, 0.0);
        let odd_negative = alternating(-0.0, -1.0);
        assert_eq!(
            encoder
                .similarity(&even_positive, &odd_negative)
                .unwrap()
                .to_bits(),
            0.0_f64.to_bits(),
            "a dot product of only negative zeros stays positive zero",
        );
        let probes = [
            facts[0].clone(),
            facts[2].clone(),
            facts[4].clone(),
            texts[3].clone(),
            vec![0.0; HolographicEncoder::DIMENSIONS],
            scaled(&facts[1], 1e-11),
            scaled(&facts[1], 1e-9),
            scaled(&facts[4], 1e200),
            nonfinite,
            infinite,
            signed_zero,
            even_positive,
            odd_negative,
        ];
        let mut similarities = Vec::new();
        for left in &probes {
            let query = encoder.prepare_query(left).unwrap();
            for right in &probes {
                let similarity = encoder.similarity(left, right).unwrap();
                let prepared = encoder.query_similarity(&query, right).unwrap();
                assert_eq!(similarity.to_bits(), prepared.to_bits());
                similarities.push(similarity);
            }
        }

        let fact_digests: Vec<String> = facts.iter().map(|fact| bits_digest(fact)).collect();
        let text_digests: Vec<String> = texts.iter().map(|text| bits_digest(text)).collect();
        let empty_fact = "0c08ff5e22e7be2347afb9f78422791d5bbc914c49d704a52693c5aa5d049390";
        let empty_text = "e593b72f6e003502f1349297be6864ab34cb1512a6959e539e165bd5c0398c88";
        assert_eq!(
            fact_digests,
            [
                empty_fact,
                empty_fact,
                "2de528046e516045c693b05a64cae2cb4315387d66c4e7ff5d217f4e6902706b",
                "1b37fe065aec12b6900dabb6f798212da901b125eb2de36de58eb951a7a5c472",
                "d1f062a392f72a47b4bd48e2091ac17b01031ff2a2e9196afdb18775ff195213",
            ],
        );
        assert_eq!(
            text_digests,
            [
                empty_text,
                empty_text,
                "a432fed6f54c020a9295fa4aa163994ad8fe647751b29e57e4a0611a94b38114",
                "8b31db77b36075fbfc50140812250700d12b2c6a112c42fe01e77e9eba2145ea",
                "01126c766c9a8c83f1c51f791be3c8492cfbaacdada83d4bd143a9c0bf3fc51c",
            ],
        );
        assert_eq!(
            bits_digest(&similarities),
            "29466a342b496755893a2ede39286af2a84d6216fa2a47e93852fec9b0eb05bd",
        );
    }

    #[test]
    fn prepared_queries_reject_noncanonical_dimensions() {
        let encoder = HolographicEncoder::new();
        let mismatch = |actual| HolographicEncodingError::DimensionMismatch {
            expected: HolographicEncoder::DIMENSIONS,
            actual,
        };
        let canonical = encoder.encode_text("canonical").unwrap();
        let query = encoder.prepare_query(&canonical).unwrap();

        assert_eq!(
            encoder.prepare_query(&[0.0; 2049]).err(),
            Some(mismatch(2049))
        );
        assert_eq!(
            encoder.query_similarity(&query, &[]).unwrap_err(),
            mismatch(0)
        );
        assert_eq!(
            encoder.similarity(&canonical, &[0.0; 5]).unwrap_err(),
            mismatch(5)
        );
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
