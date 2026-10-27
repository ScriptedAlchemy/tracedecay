use schemars::{JsonSchema, generate::SchemaSettings};
use serde::de::{Error as _, IntoDeserializer};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Number, Value};
use tracedecay_domain::{Confidence, FactCategoryV1};

use crate::retained_surfaces::MemoryAutomationFactEvidenceTrustBucketV1;

// Rust's trim uses Unicode White_Space. Spell that set out because JSON
// Schema's regex dialects disagree about which characters `\s` includes.
const TRUST_BUCKET_PATTERN: &str = r"^[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028-\u2029\u202f\u205f\u3000]*([lL][oO][wW]|[mM][eE][dD][iI][uU][mM]|[hH][iI][gG][hH])[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028-\u2029\u202f\u205f\u3000]*$";

/// An untrusted fact item shared by standalone reflection and combined review.
///
/// Decoding establishes its structure. Content/privacy checks, citation
/// membership, deduplication, trust policy, and apply authority remain runtime
/// responsibilities. Omitted tags and entities retain their released empty
/// meaning; explicit null is not an array.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionFactProposal {
    pub content: String,
    pub category: FactCategoryV1,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    pub trust: SessionFactTrust,
    pub source_span: SessionFactSourceSpan,
    pub reason: String,
}

/// Exactly one public evidence identity, without nullable or mixed variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum SessionFactSourceSpan {
    RawMessage {
        session_id: String,
        message_id: String,
    },
    RawStore {
        /// Signed 64-bit store identity. Prefer an integer JSON literal, which
        /// retains the full range. Decimal and exponent forms are accepted only
        /// when their parsed value is integral and within +/-9007199254740991.
        /// JSON Schema's integer bounds do not express this representation rule.
        #[serde(deserialize_with = "deserialize_store_id")]
        #[schemars(range(min = i64::MIN, max = i64::MAX))]
        store_id: i64,
    },
    SummaryNode {
        node_id: String,
    },
}

/// Numeric trust or a released model bucket normalized at the input boundary.
/// The existing bucket type keeps public evidence in its canonical string form;
/// the memory policy remains responsible for assigning representative scores.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SessionFactTrust {
    Numeric(
        #[serde(deserialize_with = "deserialize_numeric_trust")]
        #[schemars(range(min = 0.0, max = 1.0))]
        f64,
    ),
    Bucket(
        #[serde(deserialize_with = "deserialize_trust_bucket")]
        #[schemars(with = "String", pattern(TRUST_BUCKET_PATTERN))]
        MemoryAutomationFactEvidenceTrustBucketV1,
    ),
}

/// The same item schema can be embedded in either task's distinct envelope.
pub fn session_fact_proposal_schema() -> Value {
    SchemaSettings::default()
        .with(|settings| {
            settings.meta_schema = None;
            settings.inline_subschemas = true;
        })
        .into_generator()
        .into_root_schema_for::<SessionFactProposal>()
        .into()
}

fn deserialize_numeric_trust<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = f64::deserialize(deserializer)?;
    Confidence::new(value).map_err(D::Error::custom)?;
    Ok(value)
}

fn deserialize_trust_bucket<'de, D>(
    deserializer: D,
) -> Result<MemoryAutomationFactEvidenceTrustBucketV1, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    MemoryAutomationFactEvidenceTrustBucketV1::deserialize(
        value.trim().to_ascii_lowercase().into_deserializer(),
    )
}

fn deserialize_store_id<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    let number = Number::deserialize(deserializer)?;
    if let Some(value) = number.as_i64() {
        return Ok(value);
    }
    // Adjacent integers can share a binary64 value outside this range. Keep
    // exact i64 literals above, but refuse ambiguous large floating identities.
    // This checks the parsed Number, not its original token: serde_json can
    // already have rounded a tiny fraction such as 7.0000000000000001 to 7.0.
    const MAX_FLOAT_STORE_ID: f64 = 9_007_199_254_740_991.0;
    number
        .as_f64()
        .filter(|value| {
            value.is_finite()
                && value.fract() == 0.0
                && (-MAX_FLOAT_STORE_ID..=MAX_FLOAT_STORE_ID).contains(value)
        })
        .map(|value| value as i64)
        .ok_or_else(|| {
            D::Error::custom(
                "store_id must be a signed 64-bit integer literal or an integral floating value within +/-9007199254740991",
            )
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{SessionFactProposal, SessionFactSourceSpan, SessionFactTrust};
    use crate::retained_surfaces::MemoryAutomationFactEvidenceTrustBucketV1;

    fn proposal() -> serde_json::Value {
        json!({
            "content": "Keep the project's API stable",
            "category": "project",
            "trust": 0.8,
            "source_span": {"store_id": 7},
            "reason": "The user asked for a stable public API"
        })
    }

    #[test]
    fn omitted_arrays_decode_as_empty_but_null_is_rejected() {
        let decoded = serde_json::from_value::<SessionFactProposal>(proposal()).unwrap();
        assert!(decoded.tags.is_empty());
        assert!(decoded.entities.is_empty());
        for field in ["tags", "entities"] {
            let mut value = proposal();
            value[field] = serde_json::Value::Null;
            assert!(serde_json::from_value::<SessionFactProposal>(value).is_err());
        }
    }

    #[test]
    fn legacy_trust_labels_normalize_to_the_public_bucket() {
        let mut value = proposal();
        value["trust"] = json!("\u{0085}\u{2003} HIGH \u{3000}");
        let decoded = serde_json::from_value::<SessionFactProposal>(value).unwrap();
        assert_eq!(
            decoded.trust,
            SessionFactTrust::Bucket(MemoryAutomationFactEvidenceTrustBucketV1::High)
        );
        assert_eq!(serde_json::to_value(decoded).unwrap()["trust"], "high");
    }

    #[test]
    fn numeric_trust_retains_its_value_and_rejects_out_of_range_scores() {
        for trust in [0.0, 0.123456789, 1.0] {
            let mut value = proposal();
            value["trust"] = json!(trust);
            assert_eq!(
                serde_json::from_value::<SessionFactProposal>(value)
                    .unwrap()
                    .trust,
                SessionFactTrust::Numeric(trust)
            );
        }
        for trust in [-0.1, 1.1] {
            let mut value = proposal();
            value["trust"] = json!(trust);
            assert!(serde_json::from_value::<SessionFactProposal>(value).is_err());
        }
    }

    #[test]
    fn numeric_store_identity_normalizes_integral_values_without_saturation() {
        for store_id in [json!(7), json!(7.0), json!(i64::MIN), json!(i64::MAX)] {
            let mut value = proposal();
            value["source_span"]["store_id"] = store_id.clone();
            let decoded = serde_json::from_value::<SessionFactProposal>(value).unwrap();
            let expected = store_id.as_i64().unwrap_or(7);
            assert_eq!(
                decoded.source_span,
                SessionFactSourceSpan::RawStore { store_id: expected }
            );
        }
        for store_id in [
            json!("7"),
            json!(7.5),
            json!(9_223_372_036_854_775_808_u64),
            json!(-18_446_744_073_709_551_616.0),
            serde_json::Value::Null,
        ] {
            let mut value = proposal();
            value["source_span"]["store_id"] = store_id;
            assert!(serde_json::from_value::<SessionFactProposal>(value).is_err());
        }
    }

    #[test]
    fn raw_store_numbers_preserve_i64_literals_and_bounded_integral_floats() {
        for (number, expected) in [
            ("-9223372036854775808", i64::MIN),
            ("9223372036854775807", i64::MAX),
            ("9007199254740993", 9_007_199_254_740_993),
            ("7.0", 7),
            ("7e0", 7),
            ("70e-1", 7),
            ("-0.0", 0),
            ("9007199254740991.0", 9_007_199_254_740_991),
            ("-9007199254740991.0", -9_007_199_254_740_991),
        ] {
            // The backend materializes Value before the proposal decoder runs.
            let source: serde_json::Value =
                serde_json::from_str(&format!(r#"{{"store_id":{number}}}"#)).unwrap();
            assert_eq!(
                serde_json::from_value::<SessionFactSourceSpan>(source).unwrap(),
                SessionFactSourceSpan::RawStore { store_id: expected },
                "{number}",
            );
        }
    }

    #[test]
    fn raw_store_numbers_reject_overflow_and_ambiguous_large_floats() {
        for number in [
            "-9223372036854775809",
            "9223372036854775808",
            "9007199254740992.0",
            "-9007199254740992.0",
            "9007199254740993.0",
            "-9007199254740993.0",
            "9007199254740991.5",
            "-9007199254740991.5",
            "-9223372036854775808.0",
            "9223372036854775807.0",
        ] {
            let source: serde_json::Value =
                serde_json::from_str(&format!(r#"{{"store_id":{number}}}"#)).unwrap();
            assert!(
                serde_json::from_value::<SessionFactSourceSpan>(source).is_err(),
                "{number} must not select a rounded store identity",
            );
        }
    }

    #[test]
    fn source_alternatives_reject_unknown_mixed_and_incomplete_identities() {
        for source_span in [
            json!({"session_id": "session"}),
            json!({"store_id": 7, "node_id": "node"}),
            json!({"store_id": 7, "node_id": null}),
            json!({"store_id": 7, "unknown": true}),
            json!({}),
        ] {
            let mut value = proposal();
            value["source_span"] = source_span;
            assert!(serde_json::from_value::<SessionFactProposal>(value).is_err());
        }
    }
}
