use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use tracedecay_domain::{
    AccessPolicyDigest, AnchorResolutionStateV2, AnchorSourceGenerationV2,
    AuthorizedAnchorResolutionV2, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationIdV1,
    CanonicalObservationRelationsV1, CapabilityId, CoverageReportV1, EvidenceClass, ManifestDigest,
    ObservationId, ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
    ObservationSourceRangeV1, PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId,
    ProjectionGenerationId, ProviderId, ResolutionAuthorizationV1, RetentionClass,
    RetrievalAnchorRecord, RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2,
    ScopeResolutionId, SessionId, UtcMicros, VectorWatermark,
};

use super::*;

pub(super) fn fixture_json() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    fs::read_to_string(path).unwrap()
}

pub(super) fn fixture() -> ResearchAnchorFixtureV1 {
    decode_fixture(&fixture_json()).unwrap()
}

pub(super) fn decode_fixture(json: &str) -> Result<ResearchAnchorFixtureV1, String> {
    let raw: RawResearchAnchorFixtureV1 = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let resolver = CaptureReceiptResolver::from_receipts(&raw.sanitization_receipts)?;
    let envelope = decode_envelope(raw.envelope, &resolver)?;
    Ok(ResearchAnchorFixtureV1 {
        envelope,
        tombstones: raw.tombstones,
        sanitization_receipts: raw.sanitization_receipts,
    })
}

pub(super) fn decode_envelope(
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

pub(super) fn valid_fixture() -> ResearchAnchorFixtureV1 {
    let fixture = fixture();
    assert_eq!(
        fixture.envelope.manifest.digest,
        fixture.envelope.manifest.compute_digest().unwrap(),
        "frozen research manifest digest drifted"
    );
    fixture.envelope.validate().unwrap();
    for tombstone in &fixture.tombstones {
        tombstone
            .validate_against(&fixture.envelope.retrieval_catalog)
            .unwrap();
    }
    fixture
}

pub(super) fn refresh_catalog_snapshot_digest(fixture: &mut ResearchAnchorFixtureV1) {
    let digest = fixture.envelope.retrieval_catalog.compute_digest().unwrap();
    fixture.envelope.retrieval_catalog.snapshot.digest = digest.clone();
    for record in fixture.envelope.retrieval_catalog.records.values_mut() {
        record.capability_catalog.digest = digest.clone();
    }
    fixture.envelope.manifest.catalog_snapshot.digest = digest;
}

pub(super) fn assert_unknown_field_rejected(json: &str, field: &str) {
    let message = decode_fixture(json).unwrap_err();
    assert!(
        message.contains(&format!("unknown field `{field}`")),
        "expected `{field}` to be rejected as unknown, got: {message}"
    );
}

pub(super) fn assert_duplicate_field_rejected(json: &str, field: &str) {
    let error = serde_json::from_str::<StrictResearchAnchorFixtureV1>(json).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(&format!("duplicate field `{field}`")),
        "expected duplicate `{field}` to be rejected, got: {message}"
    );
}

const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

pub(super) fn canonical_envelope() -> CanonicalObservationEnvelopeV1 {
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new("provider.corpus").unwrap(),
        "message",
        ObservationId::new("record.corpus").unwrap(),
        CanonicalObservationRelationsV1::new(SessionId::new("session.corpus").unwrap()),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": "synthetic corpus evidence"}),
            model: Some("model.corpus".into()),
            timestamp: Some(1_762_000_000),
        }],
        CanonicalObservationEvidenceV1::new(
            ObservationOrderingDomainV1::SnapshotOrder,
            ObservationSourceRangeV1::new(4, 5).unwrap(),
        ),
    )
    .unwrap()
}

pub(super) fn canonical_anchor() -> RetrievalAnchorRecord {
    let observation_id = CanonicalObservationIdV1::new(DIGEST_A).unwrap();
    RetrievalAnchorRecord::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::ExactObservation(observation_id.clone()),
        owner: ObservationScopeV1::Project {
            project_id: ProjectId::new("project.corpus").unwrap(),
        },
        aliases: vec![],
        occurred_at: None,
        ingested_at: UtcMicros(1_762_000_000_000_000),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::Observation(
            ObservationSourceGenerationV1::new(7).unwrap(),
        ),
        projection_generation: ProjectionGenerationId::new("projection.corpus").unwrap(),
        projection_watermark: VectorWatermark {
            components: BTreeMap::from([(ShardId::new("shard.corpus").unwrap(), 7)]),
        },
        coverage: CoverageReportV1::default(),
        source_observations: vec![observation_id],
        source_anchors: vec![],
        authorization: ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.corpus").unwrap(),
            privacy_domain_id: PrivacyDomainId::new("privacy.corpus").unwrap(),
            access_policy_digest: AccessPolicyDigest::new(DIGEST_A).unwrap(),
            capability_id: CapabilityId::new("capability.corpus").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
        },
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.corpus").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap()
}

pub(super) fn authorized_resolution(
    anchor: &RetrievalAnchorRecord,
    payload_access: PayloadAccessState,
    observed: VectorWatermark,
) -> AuthorizedAnchorResolutionV2 {
    let watermark =
        FrozenWatermarkResolutionV1::new(anchor.projection_watermark().clone(), observed);
    AuthorizedAnchorResolutionV2::new(
        anchor.anchor_id().clone(),
        anchor.authorization().clone(),
        watermark.clone(),
        anchor.coverage().clone(),
        AnchorResolutionStateV2::classify(payload_access, watermark.drift),
        payload_access,
        ManifestDigest::new(DIGEST_B).unwrap(),
    )
    .unwrap()
}
