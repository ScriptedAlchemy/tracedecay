use std::collections::{BTreeMap, BTreeSet};

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

mod strict_wire;

use strict_wire::CheckedJsonValue;

impl<'de> Deserialize<'de> for ResearchAnchorTombstoneV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = CheckedJsonValue::deserialize(deserializer)?.0;
        let wire: ResearchAnchorTombstoneWireV1 =
            serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        let tombstone = Self::from(wire);
        tombstone.validate().map_err(serde::de::Error::custom)?;
        Ok(tombstone)
    }
}

/// Strict validation boundary: a manifest is not accepted without the exact
/// external catalog snapshot whose records it references. Deserialization rejects
/// duplicate keys before typed closed-wire decoding and semantic validation.
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
        let value = CheckedJsonValue::deserialize(deserializer)?.0;
        let wire: ResearchBundleEnvelopeWireV1 =
            serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        let envelope = Self {
            manifest: wire.manifest,
            retrieval_catalog: wire.retrieval_catalog,
        };
        envelope.validate().map_err(serde::de::Error::custom)?;
        Ok(envelope)
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
