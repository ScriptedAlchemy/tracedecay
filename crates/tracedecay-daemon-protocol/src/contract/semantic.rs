use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};

/// Canonical compact wire form for a daemon-produced qualification artifact.
///
/// The daemon creates this only after genuine evaluation. The JSON wire form
/// is standard unpadded base64; a decode-and-reencode check prevents aliases
/// such as padded or alternate encodings from representing the same bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalQualificationBlob(Vec<u8>);

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CanonicalQualificationBlobError {
    #[error("semantic qualification blob is empty")]
    Empty,
    #[error("semantic qualification blob is too long: {actual} bytes exceeds {maximum}")]
    TooLong { actual: usize, maximum: usize },
    #[error("semantic qualification blob is not valid base64")]
    InvalidBase64,
    #[error("semantic qualification blob is not canonical base64")]
    NonCanonicalBase64,
}

impl CanonicalQualificationBlob {
    /// This is a bounded daemon response artifact, not an unbounded report
    /// transport. It matches the workspace's bounded artifact-payload scale.
    pub const MAX_BYTES: usize = 4 * 1024 * 1024;
    const MAX_ENCODED_BYTES: usize = (Self::MAX_BYTES * 4).div_ceil(3);

    pub fn new(bytes: Vec<u8>) -> Result<Self, CanonicalQualificationBlobError> {
        if bytes.is_empty() {
            return Err(CanonicalQualificationBlobError::Empty);
        }
        if bytes.len() > Self::MAX_BYTES {
            return Err(CanonicalQualificationBlobError::TooLong {
                actual: bytes.len(),
                maximum: Self::MAX_BYTES,
            });
        }
        Ok(Self(bytes))
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    fn from_canonical_base64(encoded: &str) -> Result<Self, CanonicalQualificationBlobError> {
        if encoded.len() > Self::MAX_ENCODED_BYTES {
            return Err(CanonicalQualificationBlobError::TooLong {
                actual: encoded.len(),
                maximum: Self::MAX_ENCODED_BYTES,
            });
        }
        let bytes = STANDARD_NO_PAD
            .decode(encoded)
            .map_err(|_| CanonicalQualificationBlobError::InvalidBase64)?;
        if STANDARD_NO_PAD.encode(&bytes) != encoded {
            return Err(CanonicalQualificationBlobError::NonCanonicalBase64);
        }
        Self::new(bytes)
    }
}

impl Serialize for CanonicalQualificationBlob {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&STANDARD_NO_PAD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for CanonicalQualificationBlob {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        Self::from_canonical_base64(&encoded).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod semantic_qualification_tests {
    use super::*;
    use super::super::{
        DaemonInvocationOperation, DaemonInvocationOutcome, DaemonInvocationProblem,
        DaemonInvocationRequest, DaemonInvocationResponse,
    };
    use tracedecay_contracts::{CancellationContext, Deadline};
    use tracedecay_domain::UtcMicros;

    #[test]
    fn semantic_qualification_wire_carries_only_the_daemon_owned_profile_selection() {
        let deadline = Deadline::new(UtcMicros(2_000)).expect("deadline");
        let cancellation =
            CancellationContext::active("cancellation.semantic-qualification.request-1")
                .expect("cancellation");
        let request = DaemonInvocationRequest::semantic_qualify(
            "request.semantic-qualification.1",
            "hybrid-conservative".to_owned(),
            UtcMicros(1_000),
            deadline,
            cancellation,
        );

        assert_eq!(
            request.operation(),
            DaemonInvocationOperation::SemanticQualify
        );
        assert_eq!(request.operation().as_str(), "semantic_qualify");
        assert!(request.requires_project());
        assert_eq!(request.validate(), Ok(()));
        let wire = serde_json::to_value(request).expect("semantic qualification wire");
        assert_eq!(wire["operation"], "semantic_qualify");
        assert_eq!(wire["evaluated_profile_id"], "hybrid-conservative");
        assert!(wire.get("candidate").is_none());
        assert!(wire.get("report").is_none());
        assert!(wire.get("snapshot_digest").is_none());
    }

    #[test]
    fn semantic_publication_wire_carries_only_the_daemon_owned_profile_selection() {
        let request = DaemonInvocationRequest::semantic_evaluate_and_publish(
            "request.semantic-evaluation.default-profile",
            "hybrid-conservative".to_owned(),
            UtcMicros(1_000),
            Deadline::new(UtcMicros(2_000)).expect("deadline"),
            CancellationContext::active("cancellation.semantic-evaluation.default-profile")
                .expect("cancellation"),
        );

        assert_eq!(request.validate(), Ok(()));
        let wire = serde_json::to_value(request).expect("semantic evaluation wire");
        assert_eq!(wire["operation"], "semantic_evaluate_and_publish");
        assert_eq!(wire["evaluated_profile_id"], "hybrid-conservative");
        assert!(
            wire.get("candidate").is_none(),
            "caller-authored candidate material must not cross the publishing wire"
        );
    }

    #[test]
    fn semantic_activation_wire_carries_only_the_profile_selection_and_rollback_intent() {
        let request = DaemonInvocationRequest::semantic_activate(
            "request.semantic-activation.default-profile",
            "hybrid-conservative".to_owned(),
            true,
            UtcMicros(1_000),
            Deadline::new(UtcMicros(2_000)).expect("deadline"),
            CancellationContext::active("cancellation.semantic-activation.default-profile")
                .expect("cancellation"),
        );

        assert_eq!(
            request.operation(),
            DaemonInvocationOperation::SemanticActivate
        );
        assert_eq!(request.operation().as_str(), "semantic_activate");
        assert!(request.requires_project());
        assert_eq!(request.validate(), Ok(()));
        let wire = serde_json::to_value(request).expect("semantic activation wire");
        assert_eq!(wire["operation"], "semantic_activate");
        assert_eq!(wire["evaluated_profile_id"], "hybrid-conservative");
        assert_eq!(wire["set_rollback"], true);
        assert!(
            wire.get("candidate").is_none() && wire.get("artifact_path").is_none(),
            "caller-authored artifact material must not cross the activation wire"
        );
    }

    #[test]
    fn semantic_activation_rejects_blank_or_padded_profile_ids() {
        for profile in ["", " hybrid-conservative", &"p".repeat(257)] {
            let request = DaemonInvocationRequest::semantic_activate(
                "request.semantic-activation.invalid-profile",
                profile.to_owned(),
                false,
                UtcMicros(1_000),
                Deadline::new(UtcMicros(2_000)).expect("deadline"),
                CancellationContext::active("cancellation.semantic-activation.invalid-profile")
                    .expect("cancellation"),
            );
            assert_eq!(
                request.validate(),
                Err(DaemonInvocationProblem::InvalidRequest),
                "profile {profile:?} must be rejected before dispatch"
            );
        }
    }

    #[test]
    fn semantic_qualification_outcome_carries_one_compact_canonical_blob() {
        let response = DaemonInvocationResponse::with_outcome(
            "request.semantic-qualification.2".to_owned(),
            DaemonInvocationOutcome::SemanticEvaluatedProfileQualified {
                qualification: CanonicalQualificationBlob::new(b"test".to_vec())
                    .expect("bounded canonical bytes"),
            },
        );

        let wire = serde_json::to_value(response).expect("semantic qualification response wire");
        assert_eq!(wire["status"], "semantic_evaluated_profile_qualified");
        assert_eq!(wire["qualification"], "dGVzdA");
        assert!(wire["qualification"].is_string());
        assert!(wire.get("qualification_bytes").is_none());
        assert!(wire.get("report").is_none());
        assert!(wire.get("snapshot_digest").is_none());
    }

    #[test]
    fn semantic_qualification_wire_rejects_noncanonical_or_malformed_blob_text() {
        let response = DaemonInvocationResponse::with_outcome(
            "request.semantic-qualification.3".to_owned(),
            DaemonInvocationOutcome::SemanticEvaluatedProfileQualified {
                qualification: CanonicalQualificationBlob::new(b"test".to_vec())
                    .expect("bounded canonical bytes"),
            },
        );
        let mut wire =
            serde_json::to_value(response).expect("semantic qualification response wire");

        wire["qualification"] = serde_json::json!("dGVzdA==");
        let padded = serde_json::from_value::<DaemonInvocationResponse>(wire.clone())
            .expect_err("padded base64 is not canonical wire text");
        assert!(padded.to_string().contains("base64"));

        wire["qualification"] = serde_json::json!("not base64");
        let malformed = serde_json::from_value::<DaemonInvocationResponse>(wire)
            .expect_err("malformed base64 is not a qualification blob");
        assert!(malformed.to_string().contains("base64"));

        let too_long = CanonicalQualificationBlob::from_canonical_base64(
            &"A".repeat(CanonicalQualificationBlob::MAX_ENCODED_BYTES + 1),
        )
        .expect_err("encoded qualification blobs have a strict byte bound");
        assert_eq!(
            too_long,
            CanonicalQualificationBlobError::TooLong {
                actual: CanonicalQualificationBlob::MAX_ENCODED_BYTES + 1,
                maximum: CanonicalQualificationBlob::MAX_ENCODED_BYTES,
            }
        );
    }

    #[test]
    fn semantic_qualification_blob_rejects_an_oversized_payload_before_encoding() {
        let error =
            CanonicalQualificationBlob::new(vec![0; CanonicalQualificationBlob::MAX_BYTES + 1])
                .expect_err("qualification wire blobs have a strict byte bound");
        assert_eq!(
            error,
            CanonicalQualificationBlobError::TooLong {
                actual: CanonicalQualificationBlob::MAX_BYTES + 1,
                maximum: CanonicalQualificationBlob::MAX_BYTES,
            }
        );
    }
}
