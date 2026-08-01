//! Wire and digest stability for the string-identity newtype families.
//!
//! These identities are `#[serde(transparent)]` and feed canonical digests, so
//! any change to how they are declared has to leave both the serialized form
//! and the digest over them byte-identical. One representative per family is
//! pinned here; the digests were captured from the pre-refactor tree.

use tracedecay_domain::code_intelligence::CodeGenerationId;
use tracedecay_domain::configuration::UserProfileId;
use tracedecay_domain::feedback::{FeedbackCycleId, GitHubReviewIdV1, ProximityWarningIdV1};
use tracedecay_domain::research::{EntityId, canonical_sha256};
use tracedecay_domain::retrieval::PrincipalId;

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
            serde_json::to_string(&ProximityWarningIdV1::new("warn-1").unwrap()).unwrap(),
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

/// Round-tripping through the validating `Deserialize` returns the same value.
#[test]
fn identity_families_round_trip() {
    let entity = EntityId::new("entity-1").unwrap();
    let encoded = serde_json::to_string(&entity).unwrap();
    let decoded: EntityId = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, entity);

    let profile = UserProfileId::new("profile-1").unwrap();
    let encoded = serde_json::to_string(&profile).unwrap();
    let decoded: UserProfileId = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, profile);
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
        ProximityWarningIdV1::new("warn-1").unwrap(),
        GitHubReviewIdV1::new("review-1").unwrap(),
    ))
    .unwrap();
    assert_eq!(
        digest.as_str(),
        "sha256:4dcf315d8b4836ebf368d2f7aa8ba6b5f27af5a0c0f876ff51153da07b2a93d5"
    );
}

/// The rejection boundary is unchanged: empty, untrimmed, control-bearing, and
/// over-long values stay rejected, and a 512-byte value stays accepted.
#[test]
fn identity_families_reject_the_same_values() {
    for bad in ["", " lead", "trail ", "in\tner", "\u{7f}"] {
        assert!(EntityId::new(bad).is_err(), "accepted {bad:?}");
        assert!(CodeGenerationId::new(bad).is_err(), "accepted {bad:?}");
        assert!(PrincipalId::new(bad).is_err(), "accepted {bad:?}");
        assert!(UserProfileId::new(bad).is_err(), "accepted {bad:?}");
        assert!(FeedbackCycleId::new(bad).is_err(), "accepted {bad:?}");
        assert!(ProximityWarningIdV1::new(bad).is_err(), "accepted {bad:?}");
        assert!(GitHubReviewIdV1::new(bad).is_err(), "accepted {bad:?}");
    }
    assert!(EntityId::new("x".repeat(512)).is_ok());
    assert!(EntityId::new("x".repeat(513)).is_err());
    assert!(UserProfileId::new("x".repeat(512)).is_ok());
    assert!(UserProfileId::new("x".repeat(513)).is_err());
    assert!(PrincipalId::new("x".repeat(512)).is_ok());
    assert!(PrincipalId::new("x".repeat(513)).is_err());
}
