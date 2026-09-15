use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::DomainError;
use super::id::{ComponentVersion, SanitizationReceiptId};

/// Reference to a capture-owned sanitization receipt.
///
/// Receipt references are the explicit boundary between untrusted wire data and
/// the proof-carrying text types below. They do not claim that the domain crate
/// ran a sanitizer; the capture layer owns issuance and persistence of receipts.
#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(deny_unknown_fields)]
pub struct SanitizationReceiptRefV1 {
    receipt_id: SanitizationReceiptId,
    sanitizer_version: ComponentVersion,
}

impl SanitizationReceiptRefV1 {
    pub fn new(
        receipt_id: SanitizationReceiptId,
        sanitizer_version: ComponentVersion,
    ) -> Result<Self, DomainError> {
        receipt_id.validate()?;
        sanitizer_version.validate()?;
        Ok(Self {
            receipt_id,
            sanitizer_version,
        })
    }

    pub fn receipt_id(&self) -> &SanitizationReceiptId {
        &self.receipt_id
    }

    pub fn sanitizer_version(&self) -> &ComponentVersion {
        &self.sanitizer_version
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.receipt_id.validate()?;
        self.sanitizer_version.validate()
    }
}

/// Receipt-bound proof that runtime text passed the capture-owned sanitizer.
///
/// The proof cannot be deserialized or constructed from string parts. Callers
/// must cross the explicit receipt boundary first.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct SanitizationProofV1(SanitizationReceiptRefV1);

impl SanitizationProofV1 {
    fn from_verified_receipt(receipt: SanitizationReceiptRefV1) -> Self {
        Self(receipt)
    }

    pub fn receipt(&self) -> &SanitizationReceiptRefV1 {
        &self.0
    }

    pub fn receipt_id(&self) -> &SanitizationReceiptId {
        self.0.receipt_id()
    }

    pub fn sanitizer_version(&self) -> &ComponentVersion {
        self.0.sanitizer_version()
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.0.validate()
    }
}

impl<'de> Deserialize<'de> for SanitizationProofV1 {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Err(serde::de::Error::custom(
            "SanitizationProofV1 requires an explicit receipt boundary",
        ))
    }
}

/// Trusted capture-layer boundary for exchanging an untrusted receipt reference
/// for a sanitization proof.
///
/// This is an unsafe trait so ordinary safe callers cannot mint proofs by supplying
/// a permissive resolver. Implementations belong next to the capture-owned receipt
/// store, not in wire-decoding or general domain code.
///
/// # Safety
///
/// An implementation must reject the request unless the referenced receipt exists,
/// its sanitizer version exactly matches `receipt.sanitizer_version()`, and its
/// stored digest matches a digest computed from the exact bytes of `value`.
pub unsafe trait SanitizationReceiptResolverV1 {
    fn verify_receipt_binding(
        &self,
        receipt: &SanitizationReceiptRefV1,
        value: &str,
    ) -> Result<(), DomainError>;
}

/// Untrusted wire representation of text plus a sanitization receipt reference.
///
/// Deserializing this type does not establish that the value was sanitized. It
/// must be resolved through a capture-owned [`SanitizationReceiptResolverV1`]
/// before it can become [`SanitizedTextV1`] or [`LogSafeText`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SanitizedTextRefV1 {
    value: String,
    receipt: SanitizationReceiptRefV1,
}

impl SanitizedTextRefV1 {
    pub fn new(value: impl Into<String>, receipt: SanitizationReceiptRefV1) -> Self {
        Self {
            value: value.into(),
            receipt,
        }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn receipt(&self) -> &SanitizationReceiptRefV1 {
        &self.receipt
    }

    pub fn resolve<R>(self, resolver: &R) -> Result<SanitizedTextV1, DomainError>
    where
        R: SanitizationReceiptResolverV1 + ?Sized,
    {
        SanitizedTextV1::resolve(self, resolver)
    }
}

/// Sanitized runtime text paired with the receipt that established the proof.
///
/// Context-free `Deserialize` always rejects this trusted type. Decode
/// [`SanitizedTextRefV1`] first, then resolve it against the capture-owned receipt
/// store so the proof is bound to these exact text bytes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SanitizedTextV1 {
    value: String,
    proof: SanitizationProofV1,
}

impl SanitizedTextV1 {
    fn resolve<R>(candidate: SanitizedTextRefV1, resolver: &R) -> Result<Self, DomainError>
    where
        R: SanitizationReceiptResolverV1 + ?Sized,
    {
        validate_text_bounds(&candidate.value, "SanitizedTextV1")?;
        candidate.receipt.validate()?;
        resolver.verify_receipt_binding(&candidate.receipt, &candidate.value)?;

        Ok(Self {
            value: candidate.value,
            proof: SanitizationProofV1::from_verified_receipt(candidate.receipt),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.value
    }

    pub fn proof(&self) -> &SanitizationProofV1 {
        &self.proof
    }
}

impl Serialize for SanitizedTextV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct Wire<'a> {
            value: &'a str,
            receipt: &'a SanitizationReceiptRefV1,
        }

        Wire {
            value: &self.value,
            receipt: self.proof.receipt(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SanitizedTextV1 {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Err(serde::de::Error::custom(
            "SanitizedTextV1 requires SanitizedTextRefV1 plus a capture-owned receipt resolver",
        ))
    }
}

/// Runtime text already proven safe for diagnostic, log, and manifest export use.
///
/// Like [`SanitizedTextV1`], context-free deserialization always rejects this
/// trusted type.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct LogSafeText(SanitizedTextV1);

impl LogSafeText {
    pub fn from_sanitized(value: SanitizedTextV1) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn proof(&self) -> &SanitizationProofV1 {
        self.0.proof()
    }
}

impl<'de> Deserialize<'de> for LogSafeText {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Err(serde::de::Error::custom(
            "LogSafeText requires SanitizedTextRefV1 plus a capture-owned receipt resolver",
        ))
    }
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::*;

    struct FixtureReceiptResolver {
        receipt: SanitizationReceiptRefV1,
        value: String,
    }

    unsafe impl SanitizationReceiptResolverV1 for FixtureReceiptResolver {
        fn verify_receipt_binding(
            &self,
            receipt: &SanitizationReceiptRefV1,
            value: &str,
        ) -> Result<(), DomainError> {
            if receipt == &self.receipt && value == self.value {
                Ok(())
            } else {
                Err(DomainError::UnsafeText {
                    field: "fixture sanitization receipt binding",
                })
            }
        }
    }

    pub(crate) fn log_safe_text(value: impl Into<String>) -> LogSafeText {
        let value = value.into();
        let receipt = SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("fixture.sanitization-receipt").expect("valid fixture id"),
            ComponentVersion::new("fixture.sanitizer.v1").expect("valid fixture version"),
        )
        .expect("valid fixture receipt");
        let resolver = FixtureReceiptResolver {
            receipt: receipt.clone(),
            value: value.clone(),
        };
        let sanitized = SanitizedTextRefV1::new(value, receipt)
            .resolve(&resolver)
            .expect("valid fixture text");
        LogSafeText::from_sanitized(sanitized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ExactReceiptResolver {
        receipt: SanitizationReceiptRefV1,
        value: String,
    }

    unsafe impl SanitizationReceiptResolverV1 for ExactReceiptResolver {
        fn verify_receipt_binding(
            &self,
            receipt: &SanitizationReceiptRefV1,
            value: &str,
        ) -> Result<(), DomainError> {
            if receipt == &self.receipt && value == self.value {
                Ok(())
            } else {
                Err(DomainError::UnsafeText {
                    field: "test sanitization receipt binding",
                })
            }
        }
    }

    #[test]
    fn wire_receipt_requires_context_aware_resolution_for_exact_value() {
        let receipt = SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("test.sanitization-receipt").unwrap(),
            ComponentVersion::new("test.sanitizer.v1").unwrap(),
        )
        .unwrap();
        let wire = serde_json::to_value(SanitizedTextRefV1::new("redacted value", receipt.clone()))
            .unwrap();

        assert!(serde_json::from_value::<SanitizedTextV1>(wire.clone()).is_err());
        assert!(serde_json::from_value::<LogSafeText>(wire.clone()).is_err());

        let candidate: SanitizedTextRefV1 = serde_json::from_value(wire).unwrap();
        let resolver = ExactReceiptResolver {
            receipt: receipt.clone(),
            value: "redacted value".to_owned(),
        };
        let sanitized = candidate.resolve(&resolver).unwrap();
        assert_eq!(sanitized.as_str(), "redacted value");

        let replayed = SanitizedTextRefV1::new("different value", receipt);
        assert!(replayed.resolve(&resolver).is_err());

        let unregistered_receipt = SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("attacker.self-authored-receipt").unwrap(),
            ComponentVersion::new("test.sanitizer.v1").unwrap(),
        )
        .unwrap();
        let unregistered = SanitizedTextRefV1::new("private text", unregistered_receipt);
        assert!(unregistered.resolve(&resolver).is_err());
    }
}

impl fmt::Display for LogSafeText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn validate_text_bounds(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty() || value.len() > 4_096 || value.chars().any(char::is_control) {
        return Err(DomainError::UnsafeText { field });
    }
    Ok(())
}

/// Confidence stored as millionths for deterministic equality and ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Confidence(u32);

impl Confidence {
    const SCALE: f64 = 1_000_000.0;

    pub fn new(value: f64) -> Result<Self, DomainError> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(DomainError::InvalidConfidence);
        }
        Ok(Self((value * Self::SCALE).round() as u32))
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / Self::SCALE
    }

    pub fn is_certain(self) -> bool {
        self.0 == Self::SCALE as u32
    }
}

impl Serialize for Confidence {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_f64(self.as_f64())
    }
}

impl<'de> Deserialize<'de> for Confidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(f64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Evidence authority, ordered from weakest to strongest.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    Heuristic,
    Inferred,
    DerivedExact,
    UserDeclared,
    ProviderDeclared,
    Observed,
}

pub(crate) fn validate_evidence_confidence(
    evidence: EvidenceClass,
    confidence: Confidence,
) -> Result<(), DomainError> {
    if matches!(
        evidence,
        EvidenceClass::UserDeclared | EvidenceClass::ProviderDeclared | EvidenceClass::Observed
    ) && !confidence.is_certain()
    {
        return Err(DomainError::NonCertainDeclaration);
    }
    Ok(())
}
