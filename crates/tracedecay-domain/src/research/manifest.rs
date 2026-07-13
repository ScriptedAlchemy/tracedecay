use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

use super::canonical::canonical_sha256;
use super::coverage::CoverageReportV1;
use super::error::DomainError;
use super::evidence::{Confidence, EvidenceClass, LogSafeText, validate_evidence_confidence};
use super::id::{
    CommitId, ComponentVersion, ManifestDigest, ManifestId, NonEmptyUniqueVec, PrivacyDomainId,
    RefId, RepositoryId, ResearchAnchorId, ResearchManifestId, RetrievalAnchorId,
    RetrievalRecipeId, SanitizationReceiptId, SchemaVersion, SessionId, ensure_unique,
};
use super::retrieval::{ResearchContextAnchorV1, RetrievalAnchorCatalogV1, RetrievalRecipeV1};
use super::subjects::{
    ActorRef, AuditReceiptRef, CatalogSnapshotRefV1, EntityRef, ResearchAnchorSubjectV1,
};
use super::time::UtcMicros;
use super::watermark::VectorWatermark;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrivateCorpusManifestRef {
    pub manifest_id: ManifestId,
    pub manifest_digest: ManifestDigest,
    pub privacy_domain: PrivacyDomainId,
    pub source_watermark: VectorWatermark,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ContributionRoleV1 {
    Authored,
    Researched,
    Reviewed,
    Audited,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResearchContributionV1 {
    pub contributor: ActorRef,
    pub session_id: Option<SessionId>,
    pub role: ContributionRoleV1,
    pub outputs: Vec<EntityRef>,
    pub manifest_entries: Vec<ResearchAnchorId>,
    pub evidence_class: EvidenceClass,
    pub confidence: Confidence,
}

impl ResearchContributionV1 {
    fn validate(&self) -> Result<(), DomainError> {
        self.contributor.actor_id.validate()?;
        ensure_unique(
            self.outputs.iter().map(|value| &value.id),
            "contribution outputs",
        )?;
        ensure_unique(
            self.manifest_entries.iter(),
            "contribution manifest_entries",
        )?;
        validate_evidence_confidence(self.evidence_class, self.confidence)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AttributionGapReasonV1 {
    MissingParentToolUse,
    CopiedCoordinationText,
    CaptureGap,
    AmbiguousArtifact,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttributionGap {
    pub subject: LogSafeText,
    pub candidate_sessions: Vec<SessionId>,
    pub reason: AttributionGapReasonV1,
    pub repair_recipe: Option<RetrievalRecipeId>,
}

impl AttributionGap {
    fn validate(&self) -> Result<(), DomainError> {
        ensure_unique(self.candidate_sessions.iter(), "candidate_sessions")?;
        for session in &self.candidate_sessions {
            session.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RedactionReport {
    pub sanitizer_version: ComponentVersion,
    pub scanned: u64,
    pub redacted: u64,
    pub rejected: u64,
    pub receipts: Vec<SanitizationReceiptId>,
}

impl RedactionReport {
    fn validate(&self) -> Result<(), DomainError> {
        self.sanitizer_version.validate()?;
        if self.redacted > self.scanned || self.rejected > self.scanned {
            return Err(DomainError::InvalidRedactionCounts);
        }
        ensure_unique(self.receipts.iter(), "redaction receipts")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitTruthManifest {
    pub repository: RepositoryId,
    pub head_commit: CommitId,
    pub merge_base: Option<CommitId>,
    pub refs: Vec<(RefId, CommitId)>,
    pub dirty: bool,
    pub captured_at: UtcMicros,
}

impl GitTruthManifest {
    fn validate(&self) -> Result<(), DomainError> {
        self.repository.validate()?;
        self.head_commit.validate()?;
        ensure_unique(self.refs.iter().map(|(reference, _)| reference), "git refs")?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AnchorTombstoneReasonV1 {
    Deleted,
    Expired,
    Redacted,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ResearchAnchorTombstoneV1 {
    pub entry_id: ResearchAnchorId,
    pub retrieval_anchors: NonEmptyUniqueVec<RetrievalAnchorId>,
    pub reason: AnchorTombstoneReasonV1,
    pub occurred_at: UtcMicros,
    pub subject: ResearchAnchorSubjectV1,
    pub evidence_class: EvidenceClass,
    pub snapshot: VectorWatermark,
    pub coverage: CoverageReportV1,
    pub receipt: AuditReceiptRef,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResearchAnchorTombstoneWireV1 {
    entry_id: ResearchAnchorId,
    retrieval_anchors: NonEmptyUniqueVec<RetrievalAnchorId>,
    reason: AnchorTombstoneReasonV1,
    occurred_at: UtcMicros,
    subject: ResearchAnchorSubjectV1,
    evidence_class: EvidenceClass,
    snapshot: VectorWatermark,
    coverage: CoverageReportV1,
    receipt: AuditReceiptRef,
}

impl From<ResearchAnchorTombstoneWireV1> for ResearchAnchorTombstoneV1 {
    fn from(wire: ResearchAnchorTombstoneWireV1) -> Self {
        Self {
            entry_id: wire.entry_id,
            retrieval_anchors: wire.retrieval_anchors,
            reason: wire.reason,
            occurred_at: wire.occurred_at,
            subject: wire.subject,
            evidence_class: wire.evidence_class,
            snapshot: wire.snapshot,
            coverage: wire.coverage,
            receipt: wire.receipt,
        }
    }
}

impl ResearchAnchorTombstoneV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.entry_id.validate()?;
        for anchor in self.retrieval_anchors.iter() {
            anchor.validate()?;
        }
        self.subject.validate()?;
        self.coverage.validate()?;
        self.receipt.receipt_id.validate()
    }

    pub fn validate_against(&self, catalog: &RetrievalAnchorCatalogV1) -> Result<(), DomainError> {
        self.validate()?;
        catalog.validate()?;
        for anchor in self.retrieval_anchors.iter() {
            let record = catalog.get(anchor).ok_or(DomainError::UnknownReference {
                field: "tombstone retrieval anchor",
            })?;
            if record.snapshot != self.snapshot {
                return Err(DomainError::SnapshotMismatch {
                    field: "tombstone retrieval record snapshot",
                });
            }
        }
        Ok(())
    }
}

/// Append-only manifest version tying safe claims to canonical resolver records.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResearchBundleManifestV1 {
    pub manifest_id: ResearchManifestId,
    pub schema_version: SchemaVersion,
    pub supersedes: Option<ResearchManifestId>,
    pub created_at: UtcMicros,
    pub created_by: ActorRef,
    pub parent_plan: EntityRef,
    pub repository: RepositoryId,
    pub base_commit: CommitId,
    pub plan_commit: Option<CommitId>,
    pub catalog_snapshot: CatalogSnapshotRefV1,
    pub store_watermarks: VectorWatermark,
    pub private_corpus: Option<PrivateCorpusManifestRef>,
    pub git_snapshot: GitTruthManifest,
    pub anchors: Vec<ResearchContextAnchorV1>,
    pub agent_contributions: Vec<ResearchContributionV1>,
    pub unresolved_attribution: Vec<AttributionGap>,
    pub retrieval_recipes: Vec<RetrievalRecipeV1>,
    pub redaction_report: RedactionReport,
    pub digest: ManifestDigest,
}

/// The stable V1 digest surface. Keeping this projection explicit prevents the
/// stored digest from becoming self-referential and makes future manifest
/// fields an intentional schema-version decision rather than an accidental
/// digest-format change.
#[derive(Serialize)]
struct ResearchBundleManifestDigestV1<'a> {
    manifest_id: &'a ResearchManifestId,
    schema_version: &'a SchemaVersion,
    supersedes: &'a Option<ResearchManifestId>,
    created_at: &'a UtcMicros,
    created_by: &'a ActorRef,
    parent_plan: &'a EntityRef,
    repository: &'a RepositoryId,
    base_commit: &'a CommitId,
    plan_commit: &'a Option<CommitId>,
    catalog_snapshot: &'a CatalogSnapshotRefV1,
    store_watermarks: &'a VectorWatermark,
    private_corpus: &'a Option<PrivateCorpusManifestRef>,
    git_snapshot: &'a GitTruthManifest,
    anchors: &'a [ResearchContextAnchorV1],
    agent_contributions: &'a [ResearchContributionV1],
    unresolved_attribution: &'a [AttributionGap],
    retrieval_recipes: &'a [RetrievalRecipeV1],
    redaction_report: &'a RedactionReport,
}

impl<'a> From<&'a ResearchBundleManifestV1> for ResearchBundleManifestDigestV1<'a> {
    fn from(manifest: &'a ResearchBundleManifestV1) -> Self {
        Self {
            manifest_id: &manifest.manifest_id,
            schema_version: &manifest.schema_version,
            supersedes: &manifest.supersedes,
            created_at: &manifest.created_at,
            created_by: &manifest.created_by,
            parent_plan: &manifest.parent_plan,
            repository: &manifest.repository,
            base_commit: &manifest.base_commit,
            plan_commit: &manifest.plan_commit,
            catalog_snapshot: &manifest.catalog_snapshot,
            store_watermarks: &manifest.store_watermarks,
            private_corpus: &manifest.private_corpus,
            git_snapshot: &manifest.git_snapshot,
            anchors: &manifest.anchors,
            agent_contributions: &manifest.agent_contributions,
            unresolved_attribution: &manifest.unresolved_attribution,
            retrieval_recipes: &manifest.retrieval_recipes,
            redaction_report: &manifest.redaction_report,
        }
    }
}

struct ManifestIndexes<'a> {
    entries: BTreeMap<&'a ResearchAnchorId, &'a ResearchContextAnchorV1>,
    recipes: BTreeMap<&'a RetrievalRecipeId, &'a RetrievalRecipeV1>,
    ambiguous_authorship_sessions: BTreeSet<&'a SessionId>,
}

fn every_claimed_entry_is_provider_linked(
    manifest_entries: &[ResearchAnchorId],
    is_provider_linked: impl FnMut(&ResearchAnchorId) -> bool,
) -> bool {
    !manifest_entries.is_empty() && manifest_entries.iter().all(is_provider_linked)
}

fn collect_log_safe_text_claims<'a>(
    value: &'a serde_json::Value,
    claims: &mut Vec<(&'a str, &'a str)>,
) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_log_safe_text_claims(value, claims);
            }
        }
        serde_json::Value::Object(object) => {
            if object.len() == 2
                && object.contains_key("value")
                && let Some(receipt) = object.get("receipt").and_then(serde_json::Value::as_object)
                && receipt.len() == 2
                && let (Some(receipt_id), Some(sanitizer_version)) = (
                    receipt
                        .get("receipt_id")
                        .and_then(serde_json::Value::as_str),
                    receipt
                        .get("sanitizer_version")
                        .and_then(serde_json::Value::as_str),
                )
            {
                claims.push((receipt_id, sanitizer_version));
                return;
            }
            for value in object.values() {
                collect_log_safe_text_claims(value, claims);
            }
        }
        _ => {}
    }
}

fn validate_redaction_claims_in_value(
    value: &serde_json::Value,
    report: &RedactionReport,
) -> Result<(), DomainError> {
    let mut claims = Vec::new();
    collect_log_safe_text_claims(value, &mut claims);

    let declared = report
        .receipts
        .iter()
        .map(SanitizationReceiptId::as_str)
        .collect::<BTreeSet<_>>();
    let mut used = BTreeSet::new();
    for (receipt_id, sanitizer_version) in claims {
        if !declared.contains(receipt_id) {
            return Err(DomainError::UnknownReference {
                field: "log-safe text sanitization receipt",
            });
        }
        if sanitizer_version != report.sanitizer_version.as_str() {
            return Err(DomainError::SnapshotMismatch {
                field: "log-safe text sanitizer version",
            });
        }
        used.insert(receipt_id);
    }
    if used != declared {
        return Err(DomainError::UnknownReference {
            field: "unused redaction receipt",
        });
    }
    Ok(())
}

impl ResearchBundleManifestV1 {
    /// Validate the manifest and every retrieval reference against an external,
    /// snapshot-pinned resolver catalog.
    pub fn validate(&self, catalog: &RetrievalAnchorCatalogV1) -> Result<(), DomainError> {
        self.validate_structure()?;
        catalog.validate()?;
        if catalog.snapshot != self.catalog_snapshot {
            return Err(DomainError::SnapshotMismatch {
                field: "manifest retrieval catalog",
            });
        }

        let indexes = self.build_indexes();
        for anchor in &self.anchors {
            let recipe = indexes.recipes.get(&anchor.retrieval_recipe_id).ok_or(
                DomainError::UnknownReference {
                    field: "anchor retrieval_recipe_id",
                },
            )?;
            if recipe.snapshot != anchor.snapshot {
                return Err(DomainError::SnapshotMismatch {
                    field: "anchor retrieval recipe snapshot",
                });
            }
            if !self.store_watermarks.dominates(&anchor.snapshot) {
                return Err(DomainError::SnapshotMismatch {
                    field: "anchor store watermark",
                });
            }
            for retrieval_anchor in anchor.retrieval_anchors.iter() {
                if !recipe.anchors.contains(retrieval_anchor) {
                    return Err(DomainError::UnknownReference {
                        field: "anchor retrieval recipe membership",
                    });
                }
                let record =
                    catalog
                        .get(retrieval_anchor)
                        .ok_or(DomainError::UnknownReference {
                            field: "anchor retrieval catalog record",
                        })?;
                if record.snapshot != anchor.snapshot {
                    return Err(DomainError::SnapshotMismatch {
                        field: "anchor retrieval record snapshot",
                    });
                }
            }
        }
        for recipe in &self.retrieval_recipes {
            if !self.store_watermarks.dominates(&recipe.snapshot) {
                return Err(DomainError::SnapshotMismatch {
                    field: "recipe store watermark",
                });
            }
            for anchor in recipe.anchors.iter() {
                let record = catalog.get(anchor).ok_or(DomainError::UnknownReference {
                    field: "recipe retrieval anchor",
                })?;
                if record.snapshot != recipe.snapshot {
                    return Err(DomainError::SnapshotMismatch {
                        field: "recipe retrieval record snapshot",
                    });
                }
            }
        }
        for contribution in &self.agent_contributions {
            if contribution
                .manifest_entries
                .iter()
                .any(|entry| !indexes.entries.contains_key(entry))
            {
                return Err(DomainError::UnknownReference {
                    field: "contribution manifest entry",
                });
            }
            self.validate_authorship(contribution, &indexes)?;
        }
        for gap in &self.unresolved_attribution {
            if let Some(recipe) = &gap.repair_recipe
                && !indexes.recipes.contains_key(recipe)
            {
                return Err(DomainError::UnknownReference {
                    field: "attribution repair recipe",
                });
            }
        }
        Ok(())
    }

    /// Validate local shape and invariants that do not claim resolver existence.
    pub fn validate_structure(&self) -> Result<(), DomainError> {
        self.manifest_id.validate()?;
        self.schema_version.validate()?;
        if self.supersedes.as_ref() == Some(&self.manifest_id) {
            return Err(DomainError::SelfSupersession);
        }
        self.created_by.actor_id.validate()?;
        self.parent_plan.validate()?;
        self.repository.validate()?;
        self.base_commit.validate()?;
        self.catalog_snapshot.validate()?;
        self.git_snapshot.validate()?;
        if self.git_snapshot.repository != self.repository {
            return Err(DomainError::UnknownReference {
                field: "git_snapshot.repository",
            });
        }
        self.redaction_report.validate()?;
        self.validate_redaction_claims()?;
        self.digest.validate()?;

        ensure_unique(
            self.anchors.iter().map(|anchor| &anchor.entry_id),
            "anchors",
        )?;
        ensure_unique(
            self.retrieval_recipes
                .iter()
                .map(|recipe| &recipe.recipe_id),
            "retrieval_recipes",
        )?;
        for anchor in &self.anchors {
            anchor.validate()?;
        }
        for recipe in &self.retrieval_recipes {
            recipe.validate()?;
        }
        for contribution in &self.agent_contributions {
            contribution.validate()?;
        }
        for gap in &self.unresolved_attribution {
            gap.validate()?;
        }
        Ok(())
    }

    fn validate_redaction_claims(&self) -> Result<(), DomainError> {
        let value = serde_json::to_value(self).map_err(|_| DomainError::DigestMismatch)?;
        validate_redaction_claims_in_value(&value, &self.redaction_report)
    }

    /// Compute the domain-separated canonical digest over the stable V1
    /// manifest projection, which deliberately excludes `digest` itself.
    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        #[derive(Serialize)]
        struct DigestPayload<'a> {
            domain: &'static str,
            manifest: ResearchBundleManifestDigestV1<'a>,
        }

        canonical_sha256(&DigestPayload {
            domain: "tracedecay.research-bundle-manifest.v1",
            manifest: self.into(),
        })
    }

    pub fn verify_digest(&self) -> Result<(), DomainError> {
        if self.compute_digest()? != self.digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    fn build_indexes(&self) -> ManifestIndexes<'_> {
        let entries = self
            .anchors
            .iter()
            .map(|anchor| (&anchor.entry_id, anchor))
            .collect();
        let recipes = self
            .retrieval_recipes
            .iter()
            .map(|recipe| (&recipe.recipe_id, recipe))
            .collect();
        let ambiguous_authorship_sessions = self
            .unresolved_attribution
            .iter()
            .filter(|gap| {
                matches!(
                    gap.reason,
                    AttributionGapReasonV1::MissingParentToolUse
                        | AttributionGapReasonV1::CopiedCoordinationText
                )
            })
            .flat_map(|gap| gap.candidate_sessions.iter())
            .collect();
        ManifestIndexes {
            entries,
            recipes,
            ambiguous_authorship_sessions,
        }
    }

    fn validate_authorship(
        &self,
        contribution: &ResearchContributionV1,
        indexes: &ManifestIndexes<'_>,
    ) -> Result<(), DomainError> {
        if contribution.role != ContributionRoleV1::Authored {
            return Ok(());
        }
        let Some(session_id) = &contribution.session_id else {
            return Err(DomainError::AuthorshipWithoutProviderLinkage);
        };
        if contribution.evidence_class < EvidenceClass::ProviderDeclared
            || indexes.ambiguous_authorship_sessions.contains(session_id)
        {
            return Err(DomainError::AuthorshipWithoutProviderLinkage);
        }
        let provider_linked =
            every_claimed_entry_is_provider_linked(&contribution.manifest_entries, |entry_id| {
                indexes
                    .entries
                    .get(entry_id)
                    .filter(|anchor| anchor.evidence_class >= EvidenceClass::ProviderDeclared)
                    .and_then(|anchor| anchor.provider_activity())
                    .is_some_and(|activity| &activity.session_id == session_id)
            });
        if !provider_linked {
            return Err(DomainError::AuthorshipWithoutProviderLinkage);
        }
        Ok(())
    }
}

struct ClosedJsonValue(serde_json::Value);

impl<'de> Deserialize<'de> for ClosedJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ClosedJsonValueVisitor;

        impl<'de> Visitor<'de> for ClosedJsonValueVisitor {
            type Value = ClosedJsonValue;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value without duplicate object keys")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(ClosedJsonValue(serde_json::Value::Bool(value)))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(ClosedJsonValue(serde_json::Value::Number(value.into())))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(ClosedJsonValue(serde_json::Value::Number(value.into())))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                serde_json::Number::from_f64(value)
                    .map(serde_json::Value::Number)
                    .map(ClosedJsonValue)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_string(value.to_owned())
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(ClosedJsonValue(serde_json::Value::String(value)))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(ClosedJsonValue(serde_json::Value::Null))
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                ClosedJsonValue::deserialize(deserializer)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(ClosedJsonValue(serde_json::Value::Null))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<ClosedJsonValue>()? {
                    values.push(value.0);
                }
                Ok(ClosedJsonValue(serde_json::Value::Array(values)))
            }

            fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = serde_json::Map::new();
                while let Some(key) = entries.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!("duplicate field `{key}`")));
                    }
                    let value = entries.next_value::<ClosedJsonValue>()?;
                    values.insert(key, value.0);
                }
                Ok(ClosedJsonValue(serde_json::Value::Object(values)))
            }
        }

        deserializer.deserialize_any(ClosedJsonValueVisitor)
    }
}

type StrictWireResult<T = ()> = Result<T, String>;

fn strict_object<'a>(
    value: &'a serde_json::Value,
    allowed: &[&str],
    path: &str,
) -> StrictWireResult<Option<&'a serde_json::Map<String, serde_json::Value>>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(format!("unknown field `{field}` at {path}"));
    }
    Ok(Some(object))
}

fn strict_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    path: &str,
    check: fn(&serde_json::Value, &str) -> StrictWireResult,
) -> StrictWireResult {
    if let Some(value) = object.get(field) {
        check(value, &format!("{path}.{field}"))?;
    }
    Ok(())
}

fn strict_array(
    value: &serde_json::Value,
    path: &str,
    check: fn(&serde_json::Value, &str) -> StrictWireResult,
) -> StrictWireResult {
    if let Some(values) = value.as_array() {
        for (index, value) in values.iter().enumerate() {
            check(value, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

fn strict_map_values(
    value: &serde_json::Value,
    path: &str,
    check: fn(&serde_json::Value, &str) -> StrictWireResult,
) -> StrictWireResult {
    if let Some(values) = value.as_object() {
        for (key, value) in values {
            check(value, &format!("{path}.{key}"))?;
        }
    }
    Ok(())
}

fn strict_sanitization_receipt(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["receipt_id", "sanitizer_version"], path)?;
    Ok(())
}

fn strict_audit_receipt(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["receipt_id"], path)?;
    Ok(())
}

fn strict_log_safe_text(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["receipt", "value"], path)? else {
        return Ok(());
    };
    strict_field(object, "receipt", path, strict_sanitization_receipt)
}

fn strict_vector_watermark(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["components"], path)?;
    Ok(())
}

fn strict_shard_watermark(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["outbox_sequence", "shard_id"], path)?;
    Ok(())
}

fn strict_catalog_snapshot(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["digest", "generation"], path)?;
    Ok(())
}

fn strict_entity_kind(value: &serde_json::Value, path: &str) -> StrictWireResult {
    if !value.is_object() {
        return Ok(());
    }
    let Some(object) = strict_object(value, &["other"], path)? else {
        return Ok(());
    };
    strict_field(object, "other", path, strict_log_safe_text)
}

fn strict_entity_ref(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["id", "kind"], path)? else {
        return Ok(());
    };
    strict_field(object, "kind", path, strict_entity_kind)
}

fn strict_entity_version_ref(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["entity", "version"], path)? else {
        return Ok(());
    };
    strict_field(object, "entity", path, strict_entity_ref)
}

fn strict_actor_ref(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["actor_id", "version"], path)?;
    Ok(())
}

fn strict_time_interval(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["end", "start"], path)?;
    Ok(())
}

fn strict_source_position(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let allowed = match object.get("kind").and_then(serde_json::Value::as_str) {
        Some("byte_offset") => &["end", "kind", "start"][..],
        Some("row_id") => &["kind", "row_id"][..],
        Some("sequence") => &["kind", "sequence"][..],
        Some("object_key") => &["digest", "kind"][..],
        _ => return Ok(()),
    };
    strict_object(value, allowed, path)?;
    Ok(())
}

fn strict_activity_facet(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(
        value,
        &[
            "agent_instance_id",
            "goal_id",
            "host",
            "message_id",
            "orchestration_agent_label",
            "orchestration_observation_id",
            "parent_session_id",
            "parent_tool_use_id",
            "provider",
            "session_id",
            "source_store_id",
            "thread_id",
            "turn_id",
        ],
        path,
    )?;
    Ok(())
}

fn strict_git_subject(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(
        value,
        &[
            "commit_id",
            "project_id",
            "ref_id",
            "repository_id",
            "worktree_id",
        ],
        path,
    )?;
    Ok(())
}

fn strict_delivery_subject(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["delivery_entity", "repository_id"], path)? else {
        return Ok(());
    };
    strict_field(object, "delivery_entity", path, strict_entity_ref)
}

fn strict_source_subject(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &["source_entity", "source_position", "source_store_id"],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "source_entity", path, strict_entity_ref)?;
    strict_field(object, "source_position", path, strict_source_position)
}

fn strict_web_subject(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["captured_document", "source_manifest"], path)?
    else {
        return Ok(());
    };
    strict_field(object, "captured_document", path, strict_entity_ref)?;
    strict_field(object, "source_manifest", path, strict_entity_ref)
}

fn strict_document_subject(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["document", "version"], path)? else {
        return Ok(());
    };
    strict_field(object, "document", path, strict_entity_ref)?;
    strict_field(object, "version", path, strict_entity_version_ref)
}

fn strict_research_subject(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["kind", "subject"], path)? else {
        return Ok(());
    };
    let Some(subject) = object.get("subject") else {
        return Ok(());
    };
    let subject_path = format!("{path}.subject");
    match object.get("kind").and_then(serde_json::Value::as_str) {
        Some("activity") => strict_activity_facet(subject, &subject_path),
        Some("git") => strict_git_subject(subject, &subject_path),
        Some("delivery") => strict_delivery_subject(subject, &subject_path),
        Some("source") => strict_source_subject(subject, &subject_path),
        Some("web") => strict_web_subject(subject, &subject_path),
        Some("document") => strict_document_subject(subject, &subject_path),
        _ => Ok(()),
    }
}

fn strict_evidence_retention(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["cutoffs", "evaluated_at"], path)?;
    Ok(())
}

fn strict_read_consistency(value: &serde_json::Value, path: &str) -> StrictWireResult {
    if !value.is_object() {
        return Ok(());
    }
    let Some(object) = strict_object(value, &["bounded_stale"], path)? else {
        return Ok(());
    };
    if let Some(bounded) = object.get("bounded_stale") {
        strict_object(
            bounded,
            &["max_lag_micros"],
            &format!("{path}.bounded_stale"),
        )?;
    }
    Ok(())
}

fn strict_remote_shard_coverage(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "authority_epoch",
            "authority_id",
            "cache_age_micros",
            "cache_generation",
            "cache_not_after",
            "captured_watermark",
            "pending_local_observations",
            "pending_tombstone_acks",
            "served_by_node",
            "served_by_role",
            "shard_id",
            "sync_lag_micros",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "captured_watermark", path, strict_shard_watermark)
}

fn strict_remote_coverage(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "brain_id",
            "placement_version",
            "requested_consistency",
            "shards",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(
        object,
        "requested_consistency",
        path,
        strict_read_consistency,
    )?;
    if let Some(shards) = object.get("shards") {
        strict_array(
            shards,
            &format!("{path}.shards"),
            strict_remote_shard_coverage,
        )?;
    }
    Ok(())
}

fn strict_coverage(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "freshness",
            "incompatible",
            "locked",
            "redacted",
            "remote",
            "retention_watermark",
            "searched",
            "skipped",
            "stale",
            "truncated",
            "unavailable",
            "unknown_coverage",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    if let Some(freshness) = object.get("freshness") {
        strict_map_values(
            freshness,
            &format!("{path}.freshness"),
            strict_shard_watermark,
        )?;
    }
    strict_field(
        object,
        "retention_watermark",
        path,
        strict_evidence_retention,
    )?;
    strict_field(object, "remote", path, strict_remote_coverage)
}

fn strict_private_corpus(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "manifest_digest",
            "manifest_id",
            "privacy_domain",
            "source_watermark",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "source_watermark", path, strict_vector_watermark)
}

fn strict_git_truth(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(
        value,
        &[
            "captured_at",
            "dirty",
            "head_commit",
            "merge_base",
            "refs",
            "repository",
        ],
        path,
    )?;
    Ok(())
}

fn strict_contribution(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "confidence",
            "contributor",
            "evidence_class",
            "manifest_entries",
            "outputs",
            "role",
            "session_id",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "contributor", path, strict_actor_ref)?;
    if let Some(outputs) = object.get("outputs") {
        strict_array(outputs, &format!("{path}.outputs"), strict_entity_ref)?;
    }
    Ok(())
}

fn strict_attribution_gap(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &["candidate_sessions", "reason", "repair_recipe", "subject"],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "subject", path, strict_log_safe_text)
}

fn strict_redaction_report(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(
        value,
        &[
            "receipts",
            "redacted",
            "rejected",
            "sanitizer_version",
            "scanned",
        ],
        path,
    )?;
    Ok(())
}

fn strict_retrieval_target(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["kind", "target"], path)? else {
        return Ok(());
    };
    let Some(target) = object.get("target") else {
        return Ok(());
    };
    let target_path = format!("{path}.target");
    match object.get("kind").and_then(serde_json::Value::as_str) {
        Some("entity") => strict_entity_ref(target, &target_path),
        Some("source_position") => {
            let Some(target) = strict_object(target, &["position_digest", "source"], &target_path)?
            else {
                return Ok(());
            };
            strict_field(target, "source", &target_path, strict_entity_ref)
        }
        Some("artifact") => {
            let Some(target) = strict_object(
                target,
                &["artifact", "sanitized_output_digest"],
                &target_path,
            )?
            else {
                return Ok(());
            };
            strict_field(target, "artifact", &target_path, strict_entity_ref)
        }
        _ => Ok(()),
    }
}

fn strict_expansion_recipe(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(
        value,
        &["bounded_arguments_digest", "capability_id", "expansion"],
        path,
    )?;
    Ok(())
}

fn strict_anchor_durability(value: &serde_json::Value, path: &str) -> StrictWireResult {
    if !value.is_object() {
        return Ok(());
    }
    let Some(object) = strict_object(value, &["retention_bound"], path)? else {
        return Ok(());
    };
    if let Some(retention_bound) = object.get("retention_bound") {
        strict_object(
            retention_bound,
            &["expires_at"],
            &format!("{path}.retention_bound"),
        )?;
    }
    Ok(())
}

fn strict_retrieval_record(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "access_policy_digest",
            "anchor_id",
            "canonical_request_digest",
            "capability_catalog",
            "created_at",
            "data_version_digest",
            "durability",
            "expansion_recipe",
            "immutable_source_refs",
            "payload_access",
            "privacy_domain_id",
            "projection_version",
            "provenance",
            "resolved_scope_id",
            "retention_class",
            "schema_registry_digest",
            "snapshot",
            "source_identity_class",
            "source_observations",
            "target",
            "target_kind",
            "view",
            "view_algorithm_version",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "capability_catalog", path, strict_catalog_snapshot)?;
    strict_field(object, "durability", path, strict_anchor_durability)?;
    strict_field(object, "expansion_recipe", path, strict_expansion_recipe)?;
    if let Some(sources) = object.get("immutable_source_refs") {
        strict_array(
            sources,
            &format!("{path}.immutable_source_refs"),
            strict_entity_ref,
        )?;
    }
    strict_field(object, "snapshot", path, strict_vector_watermark)?;
    strict_field(object, "target", path, strict_retrieval_target)?;
    strict_field(object, "target_kind", path, strict_entity_kind)
}

fn strict_retrieval_catalog(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["records", "snapshot"], path)? else {
        return Ok(());
    };
    if let Some(records) = object.get("records") {
        strict_map_values(records, &format!("{path}.records"), strict_retrieval_record)?;
    }
    strict_field(object, "snapshot", path, strict_catalog_snapshot)
}

fn strict_retrieval_recipe(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &["anchors", "purpose", "recipe_id", "snapshot", "use_case"],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "purpose", path, strict_log_safe_text)?;
    strict_field(object, "snapshot", path, strict_vector_watermark)
}

fn strict_research_anchor(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "confidence",
            "coverage",
            "entry_id",
            "evidence_class",
            "expected_subject",
            "occurred_window",
            "purpose",
            "related_activity",
            "retrieval_anchors",
            "retrieval_recipe_id",
            "snapshot",
            "source_observation_ids",
            "subject",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "coverage", path, strict_coverage)?;
    strict_field(object, "expected_subject", path, strict_log_safe_text)?;
    strict_field(object, "occurred_window", path, strict_time_interval)?;
    strict_field(object, "purpose", path, strict_log_safe_text)?;
    strict_field(object, "related_activity", path, strict_activity_facet)?;
    strict_field(object, "snapshot", path, strict_vector_watermark)?;
    strict_field(object, "subject", path, strict_research_subject)
}

fn strict_manifest(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "agent_contributions",
            "anchors",
            "base_commit",
            "catalog_snapshot",
            "created_at",
            "created_by",
            "digest",
            "git_snapshot",
            "manifest_id",
            "parent_plan",
            "plan_commit",
            "private_corpus",
            "redaction_report",
            "repository",
            "retrieval_recipes",
            "schema_version",
            "store_watermarks",
            "supersedes",
            "unresolved_attribution",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    if let Some(contributions) = object.get("agent_contributions") {
        strict_array(
            contributions,
            &format!("{path}.agent_contributions"),
            strict_contribution,
        )?;
    }
    if let Some(anchors) = object.get("anchors") {
        strict_array(anchors, &format!("{path}.anchors"), strict_research_anchor)?;
    }
    strict_field(object, "catalog_snapshot", path, strict_catalog_snapshot)?;
    strict_field(object, "created_by", path, strict_actor_ref)?;
    strict_field(object, "git_snapshot", path, strict_git_truth)?;
    strict_field(object, "parent_plan", path, strict_entity_ref)?;
    strict_field(object, "private_corpus", path, strict_private_corpus)?;
    strict_field(object, "redaction_report", path, strict_redaction_report)?;
    if let Some(recipes) = object.get("retrieval_recipes") {
        strict_array(
            recipes,
            &format!("{path}.retrieval_recipes"),
            strict_retrieval_recipe,
        )?;
    }
    strict_field(object, "store_watermarks", path, strict_vector_watermark)?;
    if let Some(gaps) = object.get("unresolved_attribution") {
        strict_array(
            gaps,
            &format!("{path}.unresolved_attribution"),
            strict_attribution_gap,
        )?;
    }
    Ok(())
}

fn strict_tombstone(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "coverage",
            "entry_id",
            "evidence_class",
            "occurred_at",
            "reason",
            "receipt",
            "retrieval_anchors",
            "snapshot",
            "subject",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "coverage", path, strict_coverage)?;
    strict_field(object, "receipt", path, strict_audit_receipt)?;
    strict_field(object, "snapshot", path, strict_vector_watermark)?;
    strict_field(object, "subject", path, strict_research_subject)
}

fn strict_envelope(value: &serde_json::Value) -> StrictWireResult {
    let Some(object) = strict_object(value, &["manifest", "retrieval_catalog"], "envelope")? else {
        return Ok(());
    };
    strict_field(object, "manifest", "envelope", strict_manifest)?;
    strict_field(
        object,
        "retrieval_catalog",
        "envelope",
        strict_retrieval_catalog,
    )
}

impl<'de> Deserialize<'de> for ResearchAnchorTombstoneV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = ClosedJsonValue::deserialize(deserializer)?.0;
        strict_tombstone(&value, "tombstone").map_err(serde::de::Error::custom)?;
        let wire: ResearchAnchorTombstoneWireV1 =
            serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(wire.into())
    }
}

/// Strict validation boundary: a manifest is not accepted without the exact
/// external catalog snapshot whose records it references. Deserialization first
/// rejects unknown fields throughout the closed V1 wire tree so bytes omitted
/// from validation and digest projections cannot be smuggled into fixtures.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ResearchBundleEnvelopeV1 {
    pub manifest: ResearchBundleManifestV1,
    pub retrieval_catalog: RetrievalAnchorCatalogV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResearchBundleEnvelopeWireV1 {
    manifest: ResearchBundleManifestV1,
    retrieval_catalog: RetrievalAnchorCatalogV1,
}

impl<'de> Deserialize<'de> for ResearchBundleEnvelopeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = ClosedJsonValue::deserialize(deserializer)?.0;
        strict_envelope(&value).map_err(serde::de::Error::custom)?;
        let wire: ResearchBundleEnvelopeWireV1 =
            serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(Self {
            manifest: wire.manifest,
            retrieval_catalog: wire.retrieval_catalog,
        })
    }
}

impl ResearchBundleEnvelopeV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.manifest.validate(&self.retrieval_catalog)?;
        self.manifest.verify_digest()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn report(receipts: &[&str], sanitizer_version: &str) -> RedactionReport {
        RedactionReport {
            sanitizer_version: ComponentVersion::new(sanitizer_version).unwrap(),
            scanned: 1,
            redacted: 0,
            rejected: 0,
            receipts: receipts
                .iter()
                .map(|receipt| SanitizationReceiptId::new(*receipt).unwrap())
                .collect(),
        }
    }

    fn proof_carrying_value(receipt_id: &str, sanitizer_version: &str) -> serde_json::Value {
        json!({
            "nested": [{
                "purpose": {
                    "receipt": {
                        "receipt_id": receipt_id,
                        "sanitizer_version": sanitizer_version,
                    },
                    "value": "safe synthetic text",
                }
            }]
        })
    }

    #[test]
    fn direct_authorship_requires_provider_linkage_for_every_claimed_entry() {
        let first = ResearchAnchorId::new("research-anchor-first").unwrap();
        let second = ResearchAnchorId::new("research-anchor-second").unwrap();
        let claimed = [first.clone(), second.clone()];

        assert!(!every_claimed_entry_is_provider_linked(
            &claimed,
            |entry_id| entry_id == &first
        ));
        assert!(every_claimed_entry_is_provider_linked(&claimed, |_| true));
        assert!(!every_claimed_entry_is_provider_linked(&[], |_| true));
    }

    #[test]
    fn redaction_claims_require_exact_receipt_set_and_sanitizer_version() {
        let value = proof_carrying_value("sanitization-receipt-used-001", "sanitizer-1.0.0");

        assert!(
            validate_redaction_claims_in_value(
                &value,
                &report(&["sanitization-receipt-used-001"], "sanitizer-1.0.0"),
            )
            .is_ok()
        );
        assert!(matches!(
            validate_redaction_claims_in_value(&value, &report(&[], "sanitizer-1.0.0")),
            Err(DomainError::UnknownReference {
                field: "log-safe text sanitization receipt"
            })
        ));
        assert!(matches!(
            validate_redaction_claims_in_value(
                &value,
                &report(
                    &[
                        "sanitization-receipt-used-001",
                        "sanitization-receipt-unused-001",
                    ],
                    "sanitizer-1.0.0",
                ),
            ),
            Err(DomainError::UnknownReference {
                field: "unused redaction receipt"
            })
        ));
        assert!(matches!(
            validate_redaction_claims_in_value(
                &value,
                &report(&["sanitization-receipt-used-001"], "sanitizer-2.0.0"),
            ),
            Err(DomainError::SnapshotMismatch {
                field: "log-safe text sanitizer version"
            })
        ));
    }
}
