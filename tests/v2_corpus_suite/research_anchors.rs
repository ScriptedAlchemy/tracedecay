use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tracedecay_domain::research::{
    AnchorDurabilityClass, AnchorTombstoneReasonV1, AttributionGap, CatalogGenerationId,
    ContributionRoleV1, DomainError, EntityKind, FrozenWatermarkResolutionV1, LogSafeText,
    PayloadAccessState, ResearchAnchorSubjectV1, ResearchAnchorTombstoneV1,
    ResearchBundleEnvelopeV1, ResearchBundleManifestV1, ResearchContextAnchorV1, RetrievalRecipeId,
    RetrievalRecipeV1, SanitizationReceiptRefV1, SanitizationReceiptResolverV1, SanitizedTextRefV1,
    ShardDispositionV1, ShardId, WatermarkDriftV1,
};
use tracedecay_domain::{
    AnchorResolutionStateV2, AuthorizedAnchorResolutionV2, CanonicalObservationEnvelopeV1,
    RetrievalAnchorRecord,
};

#[allow(clippy::duplicate_mod)]
#[path = "research_anchors/support.rs"]
mod support;

use support::*;

const FIXTURE: &str = "tests/fixtures/v2/research-anchor-manifest.json";
const SYNTHETIC_RECEIPT: &str = "sanitization-receipt-synthetic-001";
const SYNTHETIC_SANITIZER: &str = "synthetic-sanitizer-1.0.0";

#[derive(Debug)]
struct ResearchAnchorFixtureV1 {
    envelope: ResearchBundleEnvelopeV1,
    tombstones: Vec<ResearchAnchorTombstoneV1>,
    sanitization_receipts: Vec<CaptureSanitizationReceiptV1>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct StrictResearchAnchorFixtureV1 {
    envelope: ResearchBundleEnvelopeV1,
    tombstones: Vec<ResearchAnchorTombstoneV1>,
    sanitization_receipts: Vec<CaptureSanitizationReceiptV1>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResearchAnchorFixtureV1 {
    envelope: Value,
    tombstones: Vec<ResearchAnchorTombstoneV1>,
    sanitization_receipts: Vec<CaptureSanitizationReceiptV1>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureSanitizationReceiptV1 {
    receipt: SanitizationReceiptRefV1,
    value_sha256: BTreeSet<String>,
}

#[derive(Debug)]
struct CaptureReceiptResolver {
    bindings: BTreeMap<SanitizationReceiptRefV1, BTreeSet<String>>,
}

impl CaptureReceiptResolver {
    fn from_receipts(receipts: &[CaptureSanitizationReceiptV1]) -> Result<Self, String> {
        let mut bindings = BTreeMap::new();
        for evidence in receipts {
            if bindings
                .insert(evidence.receipt.clone(), evidence.value_sha256.clone())
                .is_some()
            {
                return Err("duplicate capture sanitization receipt".into());
            }
        }
        Ok(Self { bindings })
    }
}

// SAFETY: this fixture resolver accepts only receipt/value bindings whose exact-byte
// SHA-256 digests are recorded as capture evidence in the checked fixture.
unsafe impl SanitizationReceiptResolverV1 for CaptureReceiptResolver {
    fn verify_receipt_binding(
        &self,
        receipt: &SanitizationReceiptRefV1,
        value: &str,
    ) -> Result<(), DomainError> {
        let digest = hex::encode(Sha256::digest(value.as_bytes()));
        if self
            .bindings
            .get(receipt)
            .is_some_and(|digests| digests.contains(&digest))
        {
            Ok(())
        } else {
            Err(DomainError::UnsafeText {
                field: "capture sanitization receipt binding",
            })
        }
    }
}

#[test]
fn research_fixture_deserializes_through_strict_envelope_and_validates() {
    let fixture = valid_fixture();
    let manifest = &fixture.envelope.manifest;

    assert_eq!(manifest.anchors.len(), 7);
    assert_eq!(manifest.retrieval_recipes.len(), 7);
    assert_eq!(manifest.unresolved_attribution.len(), 2);
    assert_eq!(fixture.envelope.retrieval_catalog.records.len(), 10);
    assert_eq!(fixture.tombstones.len(), 3);
}

#[test]
fn research_manifest_round_trips_and_uses_the_domain_digest() {
    let fixture = valid_fixture();
    let envelope = &fixture.envelope;
    let serialized = serde_json::to_string(envelope).unwrap();
    let resolver = CaptureReceiptResolver::from_receipts(&fixture.sanitization_receipts).unwrap();
    let round_tripped =
        decode_envelope(serde_json::from_str(&serialized).unwrap(), &resolver).unwrap();

    assert_eq!(*envelope, round_tripped);
    assert_eq!(
        envelope.manifest.compute_digest().unwrap(),
        envelope.manifest.digest
    );
    envelope.manifest.verify_digest().unwrap();
}

#[test]
fn frozen_catalog_generation_and_record_snapshots_are_exact() {
    let fixture = valid_fixture();
    let envelope = &fixture.envelope;
    let manifest = &envelope.manifest;
    let catalog = &envelope.retrieval_catalog;

    assert_eq!(
        manifest.catalog_snapshot.generation.as_str(),
        "catalog-synthetic-001"
    );
    assert_eq!(
        manifest.catalog_snapshot.digest.as_str(),
        "sha256:74ec82e8ac9134e6108566e685b077514bf9694399937d968cb75db2d584debe"
    );
    assert_eq!(catalog.snapshot, manifest.catalog_snapshot);

    for anchor in &manifest.anchors {
        for retrieval_anchor in anchor.retrieval_anchors.iter() {
            let record = catalog
                .get(retrieval_anchor)
                .expect("fixture retrieval anchor must be cataloged");
            assert_eq!(record.snapshot, anchor.snapshot);
        }
    }
}

#[test]
fn canonical_coverage_represents_each_shard_once_with_its_disposition() {
    let fixture = valid_fixture();
    let searched_shard = ShardId::new("shard-synthetic-a").unwrap();
    let message = fixture
        .envelope
        .manifest
        .anchors
        .iter()
        .find(|anchor| anchor.entry_id.as_str() == "research-anchor-message-001")
        .unwrap();
    let tombstone = fixture
        .tombstones
        .iter()
        .find(|tombstone| tombstone.reason == AnchorTombstoneReasonV1::Redacted)
        .unwrap();

    assert_eq!(
        message.coverage.disposition(&searched_shard),
        Some(ShardDispositionV1::Searched)
    );
    assert!(message.coverage.is_complete());
    assert_eq!(
        tombstone.coverage.disposition(&searched_shard),
        Some(ShardDispositionV1::Redacted)
    );
    assert!(!tombstone.coverage.is_complete());
}

#[test]
fn frozen_watermark_reports_current_state_drift_without_mutating_manifest() {
    let fixture = valid_fixture();
    let manifest = &fixture.envelope.manifest;
    let original = manifest.clone();
    let frozen = manifest.store_watermarks.clone();
    let mut current = frozen.clone();
    *current.components.values_mut().next().unwrap() += 1;

    assert_eq!(
        current.partial_cmp_components(&frozen),
        Some(Ordering::Greater)
    );
    assert_eq!(*manifest, original);
}

#[test]
fn manifest_rejects_unknown_payload_fields() {
    let mutated = fixture_json().replacen(
        "\"manifest\": {\n      \"agent_contributions\": [",
        "\"manifest\": {\n      \"payload\": {\"raw\": \"omitted from digest\"},\n      \"agent_contributions\": [",
        1,
    );
    assert_unknown_field_rejected(&mutated, "payload");
}

#[test]
fn research_anchor_rejects_unknown_fields() {
    let mutated = fixture_json().replacen(
        "\"anchors\": [\n        {\n          \"confidence\": 1.0,",
        "\"anchors\": [\n        {\n          \"obsolete\": true,\n          \"confidence\": 1.0,",
        1,
    );
    assert_unknown_field_rejected(&mutated, "obsolete");
}

#[test]
fn research_anchor_subject_rejects_unknown_payload_fields() {
    let mutated = fixture_json().replacen(
        "\"subject\": {\n              \"agent_instance_id\":",
        "\"subject\": {\n              \"raw_prompt\": \"private text\",\n              \"agent_instance_id\":",
        1,
    );
    assert_unknown_field_rejected(&mutated, "raw_prompt");
}

#[test]
fn research_anchor_coverage_rejects_unknown_fields() {
    let mutated = fixture_json().replacen(
        "\"coverage\": {\n            \"freshness\": {",
        "\"coverage\": {\n            \"source_text\": \"private text\",\n            \"freshness\": {",
        1,
    );
    assert_unknown_field_rejected(&mutated, "source_text");
}

#[test]
fn sanitization_objects_reject_unknown_payload_fields() {
    let log_safe_text = fixture_json().replacen(
        "\"expected_subject\": {\n            \"receipt\": {",
        "\"expected_subject\": {\n            \"source_text\": \"private text\",\n            \"receipt\": {",
        1,
    );
    assert_unknown_field_rejected(&log_safe_text, "source_text");

    let receipt = fixture_json().replacen(
        "\"receipt\": {\n              \"receipt_id\":",
        "\"receipt\": {\n              \"raw_prompt\": \"private text\",\n              \"receipt_id\":",
        1,
    );
    assert_unknown_field_rejected(&receipt, "raw_prompt");
}

#[test]
fn retrieval_recipe_rejects_unknown_fields() {
    let mutated = fixture_json().replacen(
        "\"retrieval_recipes\": [\n        {\n          \"anchors\": [",
        "\"retrieval_recipes\": [\n        {\n          \"misspelled_snapshot\": {},\n          \"anchors\": [",
        1,
    );
    assert_unknown_field_rejected(&mutated, "misspelled_snapshot");
}

#[test]
fn retrieval_catalog_rejects_unknown_fields() {
    let mutated = fixture_json().replacen(
        "\"retrieval_catalog\": {\n      \"records\": {",
        "\"retrieval_catalog\": {\n      \"obsolete\": true,\n      \"records\": {",
        1,
    );
    assert_unknown_field_rejected(&mutated, "obsolete");
}

#[test]
fn retrieval_catalog_record_rejects_unknown_payload_fields() {
    let mutated = fixture_json().replacen(
        "\"retrieval-anchor-branch-session-001\": {\n          \"access_policy_digest\":",
        "\"retrieval-anchor-branch-session-001\": {\n          \"payload\": {\"raw\": \"omitted from catalog digest\"},\n          \"access_policy_digest\":",
        1,
    );
    assert_unknown_field_rejected(&mutated, "payload");
}

#[test]
fn retrieval_catalog_rejects_duplicate_map_record_keys() {
    let mutated = fixture_json().replacen(
        "\"records\": {\n        \"retrieval-anchor-branch-session-001\": {",
        "\"records\": {\n        \"retrieval-anchor-branch-session-001\": {},\n        \"retrieval-anchor-branch-session-001\": {",
        1,
    );
    assert_duplicate_field_rejected(&mutated, "retrieval-anchor-branch-session-001");
}

#[test]
fn strict_wire_rejects_unknown_fields_across_closed_payloads() {
    let fixture = fixture_json();
    let cases = [
        (
            fixture.replacen(
                "\"envelope\": {\n    \"manifest\": {",
                "\"envelope\": {\n    \"future_envelope_field\": true,\n    \"manifest\": {",
                1,
            ),
            "future_envelope_field",
        ),
        (
            fixture.replacen(
                "\"manifest\": {\n      \"agent_contributions\": [",
                "\"manifest\": {\n      \"future_manifest_field\": true,\n      \"agent_contributions\": [",
                1,
            ),
            "future_manifest_field",
        ),
        (
            fixture.replacen(
                "\"subject\": {\n              \"agent_instance_id\":",
                "\"subject\": {\n              \"future_activity_field\": true,\n              \"agent_instance_id\":",
                1,
            ),
            "future_activity_field",
        ),
        (
            fixture.replacen(
                "\"coverage\": {\n            \"freshness\": {",
                "\"coverage\": {\n            \"future_coverage_field\": true,\n            \"freshness\": {",
                1,
            ),
            "future_coverage_field",
        ),
        (
            fixture.replacen(
                "\"retrieval-anchor-branch-session-001\": {\n          \"access_policy_digest\":",
                "\"retrieval-anchor-branch-session-001\": {\n          \"future_retrieval_field\": true,\n          \"access_policy_digest\":",
                1,
            ),
            "future_retrieval_field",
        ),
        (
            fixture.replacen(
                "\"occurred_window\": {\n            \"end\":",
                "\"occurred_window\": {\n            \"future_time_field\": true,\n            \"end\":",
                1,
            ),
            "future_time_field",
        ),
    ];

    for (json, field) in cases {
        assert_unknown_field_rejected(&json, field);
    }
}

#[test]
fn strict_wire_rejects_duplicate_keys_at_every_object_depth() {
    let fixture = fixture_json();
    let cases = [
        (
            fixture.replacen(
                "\"envelope\": {\n    \"manifest\": {",
                "\"envelope\": {\n    \"manifest\": {},\n    \"manifest\": {",
                1,
            ),
            "manifest",
        ),
        (
            fixture.replacen(
                "\"receipt_id\": \"sanitization-receipt-synthetic-001\",",
                "\"receipt_id\": \"sanitization-receipt-synthetic-001\",\n              \"receipt_id\": \"sanitization-receipt-synthetic-001\",",
                1,
            ),
            "receipt_id",
        ),
        (
            fixture.replacen(
                "\"provider\": \"provider-synthetic\",",
                "\"provider\": \"provider-synthetic\",\n              \"provider\": \"provider-synthetic\",",
                1,
            ),
            "provider",
        ),
        (
            fixture.replacen(
                "\"components\": {\n              \"shard-synthetic-a\": 42",
                "\"components\": {\n              \"shard-synthetic-a\": 42,\n              \"shard-synthetic-a\": 42",
                1,
            ),
            "shard-synthetic-a",
        ),
        (
            fixture.replacen(
                "\"records\": {\n        \"retrieval-anchor-branch-session-001\": {",
                "\"records\": {\n        \"retrieval-anchor-branch-session-001\": {},\n        \"retrieval-anchor-branch-session-001\": {",
                1,
            ),
            "retrieval-anchor-branch-session-001",
        ),
        (
            fixture.replacen(
                "\"shard-synthetic-a\": {\n                \"outbox_sequence\": 42,",
                "\"shard-synthetic-a\": {\n                \"outbox_sequence\": 42,\n                \"outbox_sequence\": 42,",
                1,
            ),
            "outbox_sequence",
        ),
    ];

    for (json, field) in cases {
        assert_duplicate_field_rejected(&json, field);
    }
}

#[test]
fn strict_wire_accepts_distinct_dynamic_keys() {
    let watermark: tracedecay_domain::research::VectorWatermark =
        serde_json::from_str(r#"{"components":{"shard-synthetic-a":1,"shard-synthetic-b":2}}"#)
            .unwrap();

    assert_eq!(watermark.components.len(), 2);
    assert_eq!(
        watermark
            .components
            .get(&ShardId::new("shard-synthetic-a").unwrap()),
        Some(&1)
    );
    assert_eq!(
        watermark
            .components
            .get(&ShardId::new("shard-synthetic-b").unwrap()),
        Some(&2)
    );
}

#[test]
fn malformed_ids_are_rejected_at_the_typed_envelope_boundary() {
    let malformed = fixture_json().replacen(
        "\"manifest_id\": \"research-manifest-synthetic-001\"",
        "\"manifest_id\": \" malformed-manifest-id\"",
        1,
    );

    assert!(decode_fixture(&malformed).is_err());
}

#[test]
fn duplicate_anchor_entries_are_rejected() {
    let mut fixture = valid_fixture();
    let duplicate = fixture.envelope.manifest.anchors[0].clone();
    fixture.envelope.manifest.anchors.push(duplicate);

    assert!(matches!(
        fixture
            .envelope
            .manifest
            .validate(&fixture.envelope.retrieval_catalog),
        Err(DomainError::DuplicateId { field: "anchors" })
    ));
}

#[test]
fn self_supersession_and_missing_recipe_references_are_rejected() {
    let mut superseding = valid_fixture();
    superseding.envelope.manifest.supersedes =
        Some(superseding.envelope.manifest.manifest_id.clone());
    assert!(matches!(
        superseding
            .envelope
            .manifest
            .validate(&superseding.envelope.retrieval_catalog),
        Err(DomainError::SelfSupersession)
    ));

    let mut missing_recipe = valid_fixture();
    missing_recipe.envelope.manifest.anchors[0].retrieval_recipe_id =
        RetrievalRecipeId::new("retrieval-recipe-synthetic-missing-001").unwrap();
    assert!(matches!(
        missing_recipe
            .envelope
            .manifest
            .validate(&missing_recipe.envelope.retrieval_catalog),
        Err(DomainError::UnknownReference {
            field: "anchor retrieval_recipe_id"
        })
    ));
}

#[test]
fn missing_catalog_records_are_rejected() {
    let mut fixture = valid_fixture();
    fixture.envelope.retrieval_catalog.records.clear();
    refresh_catalog_snapshot_digest(&mut fixture);

    assert!(matches!(
        fixture.envelope.validate(),
        Err(DomainError::UnknownReference {
            field: "anchor retrieval catalog record"
        })
    ));
}

#[test]
fn catalog_snapshot_mismatch_is_rejected() {
    let mut fixture = valid_fixture();
    fixture.envelope.manifest.catalog_snapshot.generation =
        CatalogGenerationId::new("catalog-synthetic-mismatch-001").unwrap();

    assert!(matches!(
        fixture
            .envelope
            .manifest
            .validate(&fixture.envelope.retrieval_catalog),
        Err(DomainError::SnapshotMismatch {
            field: "manifest retrieval catalog"
        })
    ));
}

#[test]
fn digest_mismatch_is_rejected_after_structural_validation() {
    let mut fixture = valid_fixture();
    fixture.envelope.manifest.redaction_report.scanned += 1;

    assert!(matches!(
        fixture.envelope.validate(),
        Err(DomainError::DigestMismatch)
    ));
}

#[test]
fn copied_coordination_cannot_be_promoted_to_direct_authorship() {
    let mut fixture = valid_fixture();
    let contribution = fixture
        .envelope
        .manifest
        .agent_contributions
        .iter_mut()
        .find(|contribution| {
            contribution.contributor.actor_id.as_str() == "actor-synthetic-unknown-001"
        })
        .unwrap();
    contribution.role = ContributionRoleV1::Authored;

    assert!(matches!(
        fixture
            .envelope
            .manifest
            .validate(&fixture.envelope.retrieval_catalog),
        Err(DomainError::AuthorshipWithoutProviderLinkage)
    ));
}

#[test]
fn synthetic_text_is_bound_to_the_declared_sanitization_receipt() {
    let fixture = valid_fixture();
    let manifest = &fixture.envelope.manifest;

    assert_eq!(manifest.redaction_report.receipts.len(), 1);
    assert_eq!(
        manifest.redaction_report.receipts[0].as_str(),
        SYNTHETIC_RECEIPT
    );
    for anchor in &manifest.anchors {
        for text in [&anchor.purpose, &anchor.expected_subject] {
            assert_eq!(text.proof().receipt_id().as_str(), SYNTHETIC_RECEIPT);
            assert_eq!(
                text.proof().sanitizer_version().as_str(),
                SYNTHETIC_SANITIZER
            );
        }
    }
    for recipe in &manifest.retrieval_recipes {
        assert_eq!(
            recipe.purpose.proof().receipt_id().as_str(),
            SYNTHETIC_RECEIPT
        );
    }
    for gap in &manifest.unresolved_attribution {
        assert_eq!(gap.subject.proof().receipt_id().as_str(), SYNTHETIC_RECEIPT);
    }
}

#[test]
fn tombstone_nested_objects_reject_unknown_payload_fields() {
    let parsed: serde_json::Value = serde_json::from_str(&fixture_json()).unwrap();
    let tombstone = parsed["tombstones"][0].clone();

    let mut coverage = tombstone.clone();
    coverage["coverage"].as_object_mut().unwrap().insert(
        "source_text".into(),
        serde_json::Value::String("private text".into()),
    );
    let error = serde_json::from_value::<ResearchAnchorTombstoneV1>(coverage).unwrap_err();
    assert!(error.to_string().contains("unknown field `source_text`"));

    let mut subject = tombstone.clone();
    subject["subject"]["subject"]
        .as_object_mut()
        .unwrap()
        .insert(
            "raw_prompt".into(),
            serde_json::Value::String("private text".into()),
        );
    let error = serde_json::from_value::<ResearchAnchorTombstoneV1>(subject).unwrap_err();
    assert!(error.to_string().contains("unknown field `raw_prompt`"));

    let mut receipt = tombstone;
    receipt["receipt"].as_object_mut().unwrap().insert(
        "payload".into(),
        serde_json::Value::String("private text".into()),
    );
    let error = serde_json::from_value::<ResearchAnchorTombstoneV1>(receipt).unwrap_err();
    assert!(error.to_string().contains("unknown field `payload`"));
}

#[test]
fn tombstones_validate_against_the_catalog_and_reject_payload_material() {
    let fixture = valid_fixture();
    fixture.tombstones[0]
        .validate_against(&fixture.envelope.retrieval_catalog)
        .unwrap();

    let payload_bearing = fixture_json().replacen(
        "\"reason\": \"redacted\",",
        "\"reason\": \"redacted\",\n      \"payload\": \"synthetic payload must not be retained\",",
        1,
    );
    assert!(decode_fixture(&payload_bearing).is_err());
}

#[test]
fn tombstones_reject_retrieval_records_from_a_different_snapshot() {
    let mut fixture = valid_fixture();
    let tombstone = fixture.tombstones[0].clone();
    let tombstone_anchor = tombstone.retrieval_anchors.iter().next().unwrap().clone();
    let mismatched_snapshot = fixture
        .envelope
        .retrieval_catalog
        .records
        .values()
        .find(|record| record.snapshot != tombstone.snapshot)
        .unwrap()
        .snapshot
        .clone();
    fixture
        .envelope
        .retrieval_catalog
        .records
        .get_mut(&tombstone_anchor)
        .unwrap()
        .snapshot = mismatched_snapshot;
    refresh_catalog_snapshot_digest(&mut fixture);

    assert!(matches!(
        tombstone.validate_against(&fixture.envelope.retrieval_catalog),
        Err(DomainError::SnapshotMismatch {
            field: "tombstone retrieval record snapshot"
        })
    ));
}

#[test]
fn tombstones_reject_invalid_retrieval_catalogs() {
    let mut fixture = valid_fixture();
    let tombstone = &fixture.tombstones[0];
    let tombstone_anchor = tombstone.retrieval_anchors.iter().next().unwrap().clone();
    fixture
        .envelope
        .retrieval_catalog
        .records
        .get_mut(&tombstone_anchor)
        .unwrap()
        .capability_catalog
        .generation = CatalogGenerationId::new("catalog-synthetic-invalid-001").unwrap();

    assert_eq!(
        tombstone.validate_against(&fixture.envelope.retrieval_catalog),
        fixture.envelope.retrieval_catalog.validate()
    );
}

#[test]
fn project_rename_and_worktree_deletion_preserve_canonical_anchor_identity() {
    let fixture = valid_fixture();
    let anchor = fixture
        .envelope
        .manifest
        .anchors
        .iter()
        .find(|anchor| anchor.entry_id.as_str() == "research-anchor-branch-session-001")
        .unwrap();
    let ResearchAnchorSubjectV1::Git(subject) = &anchor.subject else {
        panic!("branch-session fixture must use a Git subject");
    };
    let retrieval_anchor = anchor.retrieval_anchors.iter().next().unwrap().clone();
    let deleted_worktree = subject.worktree_id.as_ref().expect("worktree identity");
    let original_entry_id = anchor.entry_id.clone();
    let original_catalog_digest = fixture.envelope.retrieval_catalog.compute_digest().unwrap();

    assert_eq!(
        subject.project_id.as_ref().unwrap().as_str(),
        "project-synthetic-001"
    );
    assert_eq!(deleted_worktree.as_str(), "worktree-synthetic-001");
    assert!(!fixture_json().contains("project_display_name"));
    assert!(!fixture_json().contains("worktree_path"));
    assert!(
        fixture
            .envelope
            .retrieval_catalog
            .get(&retrieval_anchor)
            .is_some()
    );
    assert_eq!(
        anchor.entry_id.as_str(),
        "research-anchor-branch-session-001"
    );
    assert_eq!(
        retrieval_anchor.as_str(),
        "retrieval-anchor-branch-session-001"
    );

    // Location lifecycle is deliberately absent from the immutable identity. A
    // deleted worktree/ref can be removed from a recovered context anchor while
    // the stable entry and catalog lookup remain valid after a project rename.
    let mut after_location_change = anchor.clone();
    let ResearchAnchorSubjectV1::Git(subject) = &mut after_location_change.subject else {
        unreachable!();
    };
    subject.worktree_id = None;
    subject.ref_id = None;
    after_location_change.validate().unwrap();

    assert_eq!(after_location_change.entry_id, original_entry_id);
    assert_eq!(
        after_location_change.retrieval_anchors.iter().next(),
        Some(&retrieval_anchor)
    );
    assert_eq!(
        fixture.envelope.retrieval_catalog.compute_digest().unwrap(),
        original_catalog_digest
    );
    fixture
        .envelope
        .retrieval_catalog
        .get(&retrieval_anchor)
        .unwrap()
        .validate()
        .unwrap();
}

#[test]
fn shard_routing_uses_watermarks_without_rekeying_canonical_anchors() {
    let fixture = valid_fixture();
    let manifest = &fixture.envelope.manifest;
    let catalog = &fixture.envelope.retrieval_catalog;

    for anchor in &manifest.anchors {
        for retrieval_anchor in anchor.retrieval_anchors.iter() {
            let record = catalog.get(retrieval_anchor).expect("cataloged anchor");
            record.validate().unwrap();
            assert_eq!(record.anchor_id, *retrieval_anchor);
            assert_eq!(record.snapshot, anchor.snapshot);
            let resolution =
                FrozenWatermarkResolutionV1::new(record.snapshot.clone(), anchor.snapshot.clone());
            resolution.validate().unwrap();
            assert_eq!(resolution.drift, WatermarkDriftV1::Exact);
            for shard in anchor.snapshot.components.keys() {
                assert!(anchor.coverage.disposition(shard).is_some());
            }
        }
    }

    for tombstone in &fixture.tombstones {
        tombstone.validate_against(catalog).unwrap();
        assert!(matches!(
            tombstone.reason,
            AnchorTombstoneReasonV1::Deleted
                | AnchorTombstoneReasonV1::Expired
                | AnchorTombstoneReasonV1::Redacted
        ));
        for retrieval_anchor in tombstone.retrieval_anchors.iter() {
            let retained = catalog.get(retrieval_anchor).unwrap();
            assert_eq!(retained.anchor_id, *retrieval_anchor);
            assert_ne!(retained.payload_access, PayloadAccessState::Eligible);
        }
    }
}

#[test]
fn expired_response_handle_is_tombstoned_and_resolves_deterministically() {
    let fixture = valid_fixture();
    let tombstone = fixture
        .tombstones
        .iter()
        .find(|value| value.entry_id.as_str() == "research-anchor-response-handle-expired-001")
        .expect("expired response-handle tombstone");
    assert_eq!(tombstone.reason, AnchorTombstoneReasonV1::Expired);
    let ResearchAnchorSubjectV1::Delivery(subject) = &tombstone.subject else {
        panic!("response handle must retain a delivery identity");
    };
    assert_eq!(subject.delivery_entity.kind, EntityKind::ResponseHandle);

    let retrieval_anchor = tombstone.retrieval_anchors.iter().next().unwrap();
    let record = fixture
        .envelope
        .retrieval_catalog
        .get(retrieval_anchor)
        .expect("expired response-handle catalog record");
    assert_eq!(record.payload_access, PayloadAccessState::RetentionExpired);
    let AnchorDurabilityClass::RetentionBound { expires_at } = &record.durability else {
        panic!("expired response handle must be retention-bound");
    };
    assert!(expires_at.0 <= tombstone.occurred_at.0);

    let exact =
        FrozenWatermarkResolutionV1::new(record.snapshot.clone(), tombstone.snapshot.clone());
    assert_eq!(exact.drift, WatermarkDriftV1::Exact);
    assert_eq!(
        exact,
        FrozenWatermarkResolutionV1::new(record.snapshot.clone(), tombstone.snapshot.clone())
    );

    let mut observed = tombstone.snapshot.clone();
    observed
        .components
        .insert(ShardId::new("shard-synthetic-a").unwrap(), 51);
    let drifted = FrozenWatermarkResolutionV1::new(record.snapshot.clone(), observed);
    assert_eq!(drifted.drift, WatermarkDriftV1::ObservedAhead);
    drifted.validate().unwrap();
}

#[test]
fn ambiguous_target_is_tombstoned_and_resolves_deterministically() {
    // A9: exercise the `Ambiguous` payload-access typed state as a resolved
    // tombstone outcome. The committed fixture carries no ambiguous record, so
    // we drive an existing tombstoned anchor into the ambiguous state in memory
    // (its target maps to more than one candidate) and refresh the catalog
    // snapshot digests so the frozen catalog stays internally consistent,
    // mirroring `tombstones_reject_retrieval_records_from_a_different_snapshot`.
    let mut fixture = valid_fixture();
    let tombstone = fixture.tombstones[0].clone();
    let anchor_id = tombstone.retrieval_anchors.iter().next().unwrap().clone();
    fixture
        .envelope
        .retrieval_catalog
        .records
        .get_mut(&anchor_id)
        .unwrap()
        .payload_access = PayloadAccessState::Ambiguous;
    refresh_catalog_snapshot_digest(&mut fixture);

    // The ambiguous tombstone still validates against the catalog and never
    // surfaces payload material: the anchor is not payload-eligible.
    tombstone
        .validate_against(&fixture.envelope.retrieval_catalog)
        .unwrap();
    let record = fixture
        .envelope
        .retrieval_catalog
        .get(&anchor_id)
        .expect("ambiguous tombstone catalog record");
    assert_eq!(record.payload_access, PayloadAccessState::Ambiguous);
    assert_ne!(record.payload_access, PayloadAccessState::Eligible);

    // Resolution against the frozen snapshot is deterministic: identical inputs
    // resolve to the same exact result every time.
    let resolution =
        FrozenWatermarkResolutionV1::new(record.snapshot.clone(), tombstone.snapshot.clone());
    assert_eq!(resolution.drift, WatermarkDriftV1::Exact);
    assert_eq!(
        resolution,
        FrozenWatermarkResolutionV1::new(record.snapshot.clone(), tombstone.snapshot.clone())
    );

    // Payload material carried on the tombstone wire is rejected outright.
    let payload_bearing = fixture_json().replacen(
        "\"reason\": \"deleted\",",
        "\"reason\": \"deleted\",\n      \"payload\": \"synthetic payload must not be retained\",",
        1,
    );
    assert!(decode_fixture(&payload_bearing).is_err());
}

#[test]
fn private_chronological_exports_and_judgments_require_external_owner_only_storage() {
    fn allowed(path: &Path, repository: &Path, unix_mode: Option<u32>) -> bool {
        // `None` is the portable fail-closed result when a platform cannot prove
        // POSIX owner-only permissions.
        !path.starts_with(repository) && unix_mode == Some(0o600)
    }

    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let committed_raw_export = repository.join("tests/fixtures/v2/raw-chronological.jsonl");
    let external_raw_export = std::env::temp_dir().join("tracedecay-private-chronological.jsonl");
    let external_judgments = std::env::temp_dir().join("tracedecay-private-judgments.jsonl");
    assert!(!allowed(&committed_raw_export, repository, Some(0o600)));
    assert!(!allowed(&external_raw_export, repository, Some(0o644)));
    assert!(!allowed(&external_raw_export, repository, None));
    assert!(allowed(&external_raw_export, repository, Some(0o600)));
    assert!(allowed(&external_judgments, repository, Some(0o600)));

    for forbidden in ["raw_chronological_export", "private_judgment_artifact"] {
        let mutated = fixture_json().replacen(
            "{\n  \"envelope\":",
            &format!("{{\n  \"{forbidden}\": \"external-only\",\n  \"envelope\":"),
            1,
        );
        assert_unknown_field_rejected(&mutated, forbidden);
    }
}

#[test]
fn canonical_observation_envelope_round_trips_and_validates() {
    let envelope = canonical_envelope();
    let encoded = serde_json::to_value(&envelope).unwrap();

    assert_eq!(
        serde_json::from_value::<CanonicalObservationEnvelopeV1>(encoded).unwrap(),
        envelope
    );
    envelope.validate().unwrap();
}

#[test]
fn canonical_observation_envelope_rejects_unknown_and_duplicate_wire_fields() {
    let mut unknown = serde_json::to_value(canonical_envelope()).unwrap();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("raw_payload".into(), Value::String("private text".into()));
    assert_decode_rejected::<CanonicalObservationEnvelopeV1>(
        unknown,
        "unknown field `raw_payload`",
    );

    let encoded = serde_json::to_string(&canonical_envelope()).unwrap();
    let duplicate = encoded.replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
    assert!(
        serde_json::from_str::<CanonicalObservationEnvelopeV1>(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate field `version`")
    );
}

#[test]
fn canonical_retrieval_anchor_round_trips_with_derived_identity() {
    let anchor = canonical_anchor();
    let encoded = serde_json::to_value(&anchor).unwrap();

    assert!(anchor.anchor_id().as_str().starts_with("retrieval.v2."));
    assert_eq!(
        serde_json::from_value::<RetrievalAnchorRecord>(encoded).unwrap(),
        anchor
    );
    anchor.validate().unwrap();
}

#[test]
fn canonical_retrieval_anchor_rejects_payload_unknown_fields_and_claimed_ids() {
    let anchor = canonical_anchor();

    let mut root_unknown = serde_json::to_value(&anchor).unwrap();
    root_unknown
        .as_object_mut()
        .unwrap()
        .insert("payload".into(), Value::String("private text".into()));
    assert_decode_rejected::<RetrievalAnchorRecord>(root_unknown, "unknown field `payload`");

    let mut target_unknown = serde_json::to_value(&anchor).unwrap();
    target_unknown["target"]
        .as_object_mut()
        .unwrap()
        .insert("source_text".into(), Value::String("private text".into()));
    assert_decode_rejected::<RetrievalAnchorRecord>(target_unknown, "unknown field `source_text`");

    let mut forged_id = serde_json::to_value(&anchor).unwrap();
    forged_id["anchor_id"] = Value::String("retrieval.v2.forged".into());
    assert_decode_rejected::<RetrievalAnchorRecord>(forged_id, "manifest digest does not match");

    let encoded = serde_json::to_string(&anchor).unwrap();
    let field = format!("\"anchor_id\":\"{}\"", anchor.anchor_id().as_str());
    let duplicate = encoded.replacen(&field, &format!("{field},{field}"), 1);
    assert!(
        serde_json::from_str::<RetrievalAnchorRecord>(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate field `anchor_id`")
    );
}

#[test]
fn authorized_resolution_preserves_freshness_and_access_as_typed_state() {
    let anchor = canonical_anchor();
    let current = authorized_resolution(
        &anchor,
        PayloadAccessState::Eligible,
        anchor.projection_watermark().clone(),
    );
    assert_eq!(current.state(), AnchorResolutionStateV2::Current);
    assert_eq!(current.watermark().drift, WatermarkDriftV1::Exact);

    let mut ahead = anchor.projection_watermark().clone();
    ahead
        .components
        .insert(ShardId::new("shard.corpus.ahead").unwrap(), 8);
    let drifted = authorized_resolution(&anchor, PayloadAccessState::Eligible, ahead);
    assert_eq!(
        drifted.state(),
        AnchorResolutionStateV2::Drifted {
            drift: WatermarkDriftV1::ObservedAhead,
        }
    );

    for (access, expected) in [
        (
            PayloadAccessState::Redacted,
            AnchorResolutionStateV2::Redacted,
        ),
        (
            PayloadAccessState::RetentionExpired,
            AnchorResolutionStateV2::Expired,
        ),
        (
            PayloadAccessState::Deleted,
            AnchorResolutionStateV2::Deleted,
        ),
        (
            PayloadAccessState::Unavailable,
            AnchorResolutionStateV2::Unavailable,
        ),
        (
            PayloadAccessState::Ambiguous,
            AnchorResolutionStateV2::Ambiguous,
        ),
    ] {
        let resolution =
            authorized_resolution(&anchor, access, anchor.projection_watermark().clone());
        assert_eq!(resolution.state(), expected);
        resolution.validate().unwrap();
    }
}

#[test]
fn authorized_resolution_rejects_unknown_fields_and_incoherent_states() {
    let anchor = canonical_anchor();
    let resolution = authorized_resolution(
        &anchor,
        PayloadAccessState::Redacted,
        anchor.projection_watermark().clone(),
    );

    let mut unknown = serde_json::to_value(&resolution).unwrap();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("payload".into(), Value::String("private text".into()));
    assert_decode_rejected::<AuthorizedAnchorResolutionV2>(unknown, "unknown field `payload`");

    let mut incoherent = serde_json::to_value(&resolution).unwrap();
    incoherent["state"] = serde_json::json!({"kind": "current"});
    assert_decode_rejected::<AuthorizedAnchorResolutionV2>(incoherent, "anchor resolution state");

    let frozen = anchor.projection_watermark().clone();
    let mut observed = frozen.clone();
    *observed.components.values_mut().next().unwrap() += 1;
    assert_eq!(
        observed.partial_cmp_components(&frozen),
        Some(Ordering::Greater)
    );
    let mut invalid_drift =
        serde_json::to_value(FrozenWatermarkResolutionV1::new(frozen, observed)).unwrap();
    invalid_drift["drift"] = Value::String("exact".into());
    assert_decode_rejected::<FrozenWatermarkResolutionV1>(invalid_drift, "frozen resolution drift");
}

fn assert_decode_rejected<T>(value: Value, expected: &str)
where
    T: serde::de::DeserializeOwned,
{
    let error = match serde_json::from_value::<T>(value) {
        Ok(_) => panic!("wire value unexpectedly decoded"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains(expected),
        "expected `{expected}` to be rejected, got: {error}"
    );
}
