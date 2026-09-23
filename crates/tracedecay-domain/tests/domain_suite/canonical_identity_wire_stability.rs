//! Wire and digest stability for the string-identity newtype families.
//!
//! These identities are `#[serde(transparent)]` and feed canonical digests, so
//! any change to how they are declared has to leave both the serialized form
//! and the digest over them byte-identical. One representative per family is
//! pinned here; the digests were captured from the pre-refactor tree.

use tracedecay_domain::code_intelligence::{CodeGenerationId, ContentDigest};
use tracedecay_domain::configuration::UserProfileId;
use tracedecay_domain::feedback::{FeedbackCycleId, GitHubReviewIdV1, ProximityContributionIdV1};
use tracedecay_domain::observation::CanonicalObservationIdV1;
use tracedecay_domain::research::{DomainError, EntityId, canonical_sha256};
use tracedecay_domain::retrieval::{PrincipalId, RetrievalContractError};
use tracedecay_domain::session::{MessageOccurrenceIdV1, ProjectionOutputOrdinalV1};

/// Every family serializes as the bare string, with no wrapper object.
#[test]
fn identity_families_serialize_transparently() {
    let cases: Vec<(String, &str)> = vec![
        (
            serde_json::to_string(&EntityId::new("entity-1").unwrap()).unwrap(),
            "\"entity-1\"",
        ),
        (
            serde_json::to_string(&CodeGenerationId::new("gen-1").unwrap()).unwrap(),
            "\"gen-1\"",
        ),
        (
            serde_json::to_string(&PrincipalId::new("principal-1").unwrap()).unwrap(),
            "\"principal-1\"",
        ),
        (
            serde_json::to_string(&UserProfileId::new("profile-1").unwrap()).unwrap(),
            "\"profile-1\"",
        ),
        (
            serde_json::to_string(&FeedbackCycleId::new("cycle-1").unwrap()).unwrap(),
            "\"cycle-1\"",
        ),
        (
            serde_json::to_string(&ProximityContributionIdV1::new("warn-1").unwrap()).unwrap(),
            "\"warn-1\"",
        ),
        (
            serde_json::to_string(&GitHubReviewIdV1::new("review-1").unwrap()).unwrap(),
            "\"review-1\"",
        ),
    ];
    for (actual, expected) in cases {
        assert_eq!(actual, expected);
    }
}

/// A canonical digest taken over one identity from each family. A change to
/// the declaration that altered the serialized bytes would move these.
#[test]
fn identity_families_digest_is_stable() {
    let digest = canonical_sha256(&(
        EntityId::new("entity-1").unwrap(),
        CodeGenerationId::new("gen-1").unwrap(),
        PrincipalId::new("principal-1").unwrap(),
        UserProfileId::new("profile-1").unwrap(),
        FeedbackCycleId::new("cycle-1").unwrap(),
        ProximityContributionIdV1::new("warn-1").unwrap(),
        GitHubReviewIdV1::new("review-1").unwrap(),
    ))
    .unwrap();
    assert_eq!(
        digest.as_str(),
        "sha256:4dcf315d8b4836ebf368d2f7aa8ba6b5f27af5a0c0f876ff51153da07b2a93d5"
    );
}

/// Derived identities that run real digest material through the shared hex
/// encoder. Unlike the pins above, the expected values are not captured from
/// any tree: they are the SHA-256 of the documented pre-image, computed
/// independently, so this fails if either the digest material or the encoding
/// moves.
///
/// `ContentDigest::of_bytes` hashes the payload alone:
///
/// ```text
/// printf 'tracedecay' | sha256sum
/// ```
///
/// `MessageOccurrenceIdV1::derive` hashes the domain separator (NUL-terminated),
/// then the observation identity, then the ordinal as big-endian `u32`:
///
/// ```text
/// printf 'tracedecay.session.message-occurrence.v1\000sha256:aaaa…aaaa\000\000\000\007' | sha256sum
/// ```
#[test]
fn derived_identities_match_their_independent_pre_image() {
    assert_eq!(
        ContentDigest::of_bytes(b"tracedecay").as_str(),
        "sha256:2d9273d4038f6fb8310e342aee294267d0ba54b30789d748b257bbc814d25e40"
    );

    let observation_id = CanonicalObservationIdV1::new(format!("sha256:{}", "a".repeat(64)))
        .expect("canonical observation identity");
    let occurrence =
        MessageOccurrenceIdV1::derive(&observation_id, ProjectionOutputOrdinalV1::new(7));
    assert_eq!(
        occurrence.as_str(),
        "sha256:a2c538568a5d1529603def303f14e9bf1189d40a5de5a7b95abddbbb02dfbd3c"
    );
}

/// The rejection boundary is unchanged: empty, untrimmed, control-bearing, and
/// over-long values stay rejected, and a 512-byte value stays accepted.
#[test]
fn identity_families_reject_the_same_values() {
    for bad in ["", " lead", "trail ", "in\tner", "\u{7f}"] {
        let empty = bad.is_empty();
        let domain = |field: &'static str| {
            if empty {
                DomainError::Empty { field }
            } else {
                DomainError::NonCanonical { field }
            }
        };
        assert_eq!(
            EntityId::new(bad).unwrap_err(),
            domain("EntityId"),
            "{bad:?}"
        );
        assert_eq!(
            CodeGenerationId::new(bad).unwrap_err(),
            DomainError::NonCanonical {
                field: "CodeGenerationId"
            },
            "{bad:?}"
        );
        assert_eq!(
            PrincipalId::new(bad).unwrap_err(),
            RetrievalContractError::InvalidIdentity {
                field: "PrincipalId"
            },
            "{bad:?}"
        );
        assert_eq!(
            UserProfileId::new(bad).unwrap_err(),
            domain("user profile id"),
            "{bad:?}"
        );
        assert_eq!(
            FeedbackCycleId::new(bad).unwrap_err(),
            domain("feedback cycle id"),
            "{bad:?}"
        );
        assert_eq!(
            ProximityContributionIdV1::new(bad).unwrap_err(),
            domain("proximity contribution id"),
            "{bad:?}"
        );
        assert_eq!(
            GitHubReviewIdV1::new(bad).unwrap_err(),
            domain("github review id"),
            "{bad:?}"
        );
    }
    let accepted = "x".repeat(512);
    assert_eq!(
        EntityId::new(accepted.clone())
            .expect("512-byte identity")
            .as_str(),
        accepted
    );
    assert_eq!(
        EntityId::new("x".repeat(513)).unwrap_err(),
        DomainError::NonCanonical { field: "EntityId" }
    );
    assert_eq!(
        UserProfileId::new(accepted.clone())
            .expect("512-byte profile id")
            .as_str(),
        accepted
    );
    assert_eq!(
        UserProfileId::new("x".repeat(513)).unwrap_err(),
        DomainError::NonCanonical {
            field: "user profile id"
        }
    );
    assert_eq!(
        PrincipalId::new(accepted)
            .expect("512-byte principal")
            .as_str(),
        "x".repeat(512)
    );
    assert_eq!(
        PrincipalId::new("x".repeat(513)).unwrap_err(),
        RetrievalContractError::InvalidIdentity {
            field: "PrincipalId"
        }
    );
}
