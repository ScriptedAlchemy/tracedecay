use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use tracedecay_domain::research::{
    AttributionGap, CatalogGenerationId, ContributionRoleV1, DomainError, LogSafeText,
    ResearchAnchorTombstoneV1, ResearchBundleEnvelopeV1, ResearchBundleManifestV1,
    ResearchContextAnchorV1, RetrievalRecipeId, RetrievalRecipeV1, SanitizationReceiptRefV1,
    SanitizationReceiptResolverV1, SanitizedTextRefV1, ShardDispositionV1, ShardId,
};

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

fn fixture_json() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    fs::read_to_string(path).unwrap()
}

fn fixture() -> ResearchAnchorFixtureV1 {
    decode_fixture(&fixture_json()).unwrap()
}

fn decode_fixture(json: &str) -> Result<ResearchAnchorFixtureV1, String> {
    let raw: RawResearchAnchorFixtureV1 = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let resolver = CaptureReceiptResolver::from_receipts(&raw.sanitization_receipts)?;
    let envelope = decode_envelope(raw.envelope, &resolver)?;
    Ok(ResearchAnchorFixtureV1 {
        envelope,
        tombstones: raw.tombstones,
        sanitization_receipts: raw.sanitization_receipts,
    })
}

fn decode_envelope(
    value: Value,
    resolver: &CaptureReceiptResolver,
) -> Result<ResearchBundleEnvelopeV1, String> {
    let mut object = into_object(value, "envelope")?;
    let manifest = decode_manifest(take_value(&mut object, "manifest")?, resolver)?;
    let retrieval_catalog = take(&mut object, "retrieval_catalog")?;
    reject_unknown(object)?;
    Ok(ResearchBundleEnvelopeV1 {
        manifest,
        retrieval_catalog,
    })
}

fn decode_manifest(
    value: Value,
    resolver: &CaptureReceiptResolver,
) -> Result<ResearchBundleManifestV1, String> {
    let mut object = into_object(value, "manifest")?;
    let anchors = take_values(&mut object, "anchors")?
        .into_iter()
        .map(|value| decode_anchor(value, resolver))
        .collect::<Result<_, _>>()?;
    let unresolved_attribution = take_values(&mut object, "unresolved_attribution")?
        .into_iter()
        .map(|value| decode_attribution_gap(value, resolver))
        .collect::<Result<_, _>>()?;
    let retrieval_recipes = take_values(&mut object, "retrieval_recipes")?
        .into_iter()
        .map(|value| decode_recipe(value, resolver))
        .collect::<Result<_, _>>()?;
    let manifest = ResearchBundleManifestV1 {
        manifest_id: take(&mut object, "manifest_id")?,
        schema_version: take(&mut object, "schema_version")?,
        supersedes: take(&mut object, "supersedes")?,
        created_at: take(&mut object, "created_at")?,
        created_by: take(&mut object, "created_by")?,
        parent_plan: take(&mut object, "parent_plan")?,
        repository: take(&mut object, "repository")?,
        base_commit: take(&mut object, "base_commit")?,
        plan_commit: take(&mut object, "plan_commit")?,
        catalog_snapshot: take(&mut object, "catalog_snapshot")?,
        store_watermarks: take(&mut object, "store_watermarks")?,
        private_corpus: take(&mut object, "private_corpus")?,
        git_snapshot: take(&mut object, "git_snapshot")?,
        anchors,
        agent_contributions: take(&mut object, "agent_contributions")?,
        unresolved_attribution,
        retrieval_recipes,
        redaction_report: take(&mut object, "redaction_report")?,
        digest: take(&mut object, "digest")?,
    };
    reject_unknown(object)?;
    Ok(manifest)
}

fn decode_anchor(
    value: Value,
    resolver: &CaptureReceiptResolver,
) -> Result<ResearchContextAnchorV1, String> {
    let mut object = into_object(value, "anchor")?;
    let anchor = ResearchContextAnchorV1 {
        entry_id: take(&mut object, "entry_id")?,
        retrieval_anchors: take(&mut object, "retrieval_anchors")?,
        purpose: resolve_text(take_value(&mut object, "purpose")?, resolver)?,
        subject: take(&mut object, "subject")?,
        related_activity: take(&mut object, "related_activity")?,
        occurred_window: take(&mut object, "occurred_window")?,
        source_observation_ids: take(&mut object, "source_observation_ids")?,
        evidence_class: take(&mut object, "evidence_class")?,
        confidence: take(&mut object, "confidence")?,
        expected_subject: resolve_text(take_value(&mut object, "expected_subject")?, resolver)?,
        retrieval_recipe_id: take(&mut object, "retrieval_recipe_id")?,
        snapshot: take(&mut object, "snapshot")?,
        coverage: take(&mut object, "coverage")?,
    };
    reject_unknown(object)?;
    Ok(anchor)
}

fn decode_recipe(
    value: Value,
    resolver: &CaptureReceiptResolver,
) -> Result<RetrievalRecipeV1, String> {
    let mut object = into_object(value, "retrieval recipe")?;
    let recipe = RetrievalRecipeV1 {
        recipe_id: take(&mut object, "recipe_id")?,
        use_case: take(&mut object, "use_case")?,
        anchors: take(&mut object, "anchors")?,
        purpose: resolve_text(take_value(&mut object, "purpose")?, resolver)?,
        snapshot: take(&mut object, "snapshot")?,
    };
    reject_unknown(object)?;
    Ok(recipe)
}

fn decode_attribution_gap(
    value: Value,
    resolver: &CaptureReceiptResolver,
) -> Result<AttributionGap, String> {
    let mut object = into_object(value, "attribution gap")?;
    let gap = AttributionGap {
        subject: resolve_text(take_value(&mut object, "subject")?, resolver)?,
        candidate_sessions: take(&mut object, "candidate_sessions")?,
        reason: take(&mut object, "reason")?,
        repair_recipe: take(&mut object, "repair_recipe")?,
    };
    reject_unknown(object)?;
    Ok(gap)
}

fn resolve_text(value: Value, resolver: &CaptureReceiptResolver) -> Result<LogSafeText, String> {
    let candidate: SanitizedTextRefV1 = serde_json::from_value(value).map_err(|e| e.to_string())?;
    candidate
        .resolve(resolver)
        .map(LogSafeText::from_sanitized)
        .map_err(|e| e.to_string())
}

fn into_object(value: Value, field: &str) -> Result<Map<String, Value>, String> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| format!("`{field}` must be an object"))
}

fn take_value(object: &mut Map<String, Value>, field: &str) -> Result<Value, String> {
    object
        .remove(field)
        .ok_or_else(|| format!("missing field `{field}`"))
}

fn take_values(object: &mut Map<String, Value>, field: &str) -> Result<Vec<Value>, String> {
    serde_json::from_value(take_value(object, field)?).map_err(|e| e.to_string())
}

fn take<T: DeserializeOwned>(object: &mut Map<String, Value>, field: &str) -> Result<T, String> {
    serde_json::from_value(take_value(object, field)?).map_err(|e| e.to_string())
}

fn reject_unknown(object: Map<String, Value>) -> Result<(), String> {
    match object.into_iter().next() {
        Some((field, _)) => Err(format!("unknown field `{field}`")),
        None => Ok(()),
    }
}

fn valid_fixture() -> ResearchAnchorFixtureV1 {
    let fixture = fixture();
    fixture.envelope.validate().unwrap();
    for tombstone in &fixture.tombstones {
        tombstone
            .validate_against(&fixture.envelope.retrieval_catalog)
            .unwrap();
    }
    fixture
}

fn refresh_catalog_snapshot_digest(fixture: &mut ResearchAnchorFixtureV1) {
    let digest = fixture.envelope.retrieval_catalog.compute_digest().unwrap();
    fixture.envelope.retrieval_catalog.snapshot.digest = digest.clone();
    for record in fixture.envelope.retrieval_catalog.records.values_mut() {
        record.capability_catalog.digest = digest.clone();
    }
    fixture.envelope.manifest.catalog_snapshot.digest = digest;
}

fn assert_unknown_field_rejected(json: &str, field: &str) {
    let error = serde_json::from_str::<StrictResearchAnchorFixtureV1>(json).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(&format!("unknown field `{field}`")),
        "expected `{field}` to be rejected as unknown, got: {message}"
    );
}

fn assert_duplicate_field_rejected(json: &str, field: &str) {
    let error = serde_json::from_str::<StrictResearchAnchorFixtureV1>(json).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(&format!("duplicate field `{field}`")),
        "expected duplicate `{field}` to be rejected, got: {message}"
    );
}

#[test]
fn research_fixture_deserializes_through_strict_envelope_and_validates() {
    let fixture = valid_fixture();
    let manifest = &fixture.envelope.manifest;

    assert_eq!(manifest.anchors.len(), 7);
    assert_eq!(manifest.retrieval_recipes.len(), 7);
    assert_eq!(manifest.unresolved_attribution.len(), 2);
    assert_eq!(fixture.envelope.retrieval_catalog.records.len(), 8);
    assert_eq!(fixture.tombstones.len(), 1);
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
        "sha256:bdfdd355330876e4abfe6897df643932ffbe9022fdbffa129cf3829aa443e679"
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
    let tombstone = fixture.tombstones[0].clone();

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
