use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use tracedecay_domain::{FactOwnerV1, ManifestDigest, canonical_sha256};

pub const MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1: &str = "tracedecay.memory-v2-owner-archive.v1";

#[derive(Clone, Copy, Debug, Deserialize, Ord, PartialOrd, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryV2ArchiveFamilyV1 {
    RetrievalAnchor,
    RetrievalAnchorAlias,
    RetrievalAnchorDisposition,
    RetrievalAnchorReverseLineage,
    RetrievalAnchorDerivativeTombstone,
    EvidenceSourceOccurrence,
    EvidenceOccurrenceSet,
    EvidenceOccurrenceSetMember,
    EvidenceSpan,
    EvidenceSpanMember,
    EvidenceSpanProjectionReceipt,
    EvidenceRetrieverContribution,
    EvidenceDerivedAnchor,
    EvidenceAssemblyReceipt,
    Fact,
    Assertion,
    AssertionSupersession,
    AssertionPayload,
    AssertionVector,
    FactEvidence,
    AssertionEvidence,
    LineageEvent,
    CurrentFact,
    LegacyFactMap,
    LegacyQuarantine,
    CompatibilityOperationReceipt,
    LegacyFeedbackEventMap,
    FeedbackHistory,
    FactRelation,
    Proposal,
    ProposalTransition,
    ProposalCurrent,
    LegacyProposalMap,
}

pub fn authoritative_memory_v2_archive_families() -> BTreeSet<MemoryV2ArchiveFamilyV1> {
    use MemoryV2ArchiveFamilyV1::{
        Assertion, AssertionEvidence, AssertionPayload, AssertionSupersession, AssertionVector,
        CompatibilityOperationReceipt, CurrentFact, EvidenceAssemblyReceipt, EvidenceDerivedAnchor,
        EvidenceOccurrenceSet, EvidenceOccurrenceSetMember, EvidenceRetrieverContribution,
        EvidenceSourceOccurrence, EvidenceSpan, EvidenceSpanMember, EvidenceSpanProjectionReceipt,
        Fact, FactEvidence, FactRelation, FeedbackHistory, LegacyFactMap, LegacyFeedbackEventMap,
        LegacyProposalMap, LegacyQuarantine, LineageEvent, Proposal, ProposalCurrent,
        ProposalTransition, RetrievalAnchor, RetrievalAnchorAlias,
        RetrievalAnchorDerivativeTombstone, RetrievalAnchorDisposition,
        RetrievalAnchorReverseLineage,
    };

    BTreeSet::from([
        RetrievalAnchor,
        RetrievalAnchorAlias,
        RetrievalAnchorDisposition,
        RetrievalAnchorReverseLineage,
        RetrievalAnchorDerivativeTombstone,
        EvidenceSourceOccurrence,
        EvidenceOccurrenceSet,
        EvidenceOccurrenceSetMember,
        EvidenceSpan,
        EvidenceSpanMember,
        EvidenceSpanProjectionReceipt,
        EvidenceRetrieverContribution,
        EvidenceDerivedAnchor,
        EvidenceAssemblyReceipt,
        Fact,
        Assertion,
        AssertionSupersession,
        AssertionPayload,
        AssertionVector,
        FactEvidence,
        AssertionEvidence,
        LineageEvent,
        CurrentFact,
        LegacyFactMap,
        LegacyQuarantine,
        CompatibilityOperationReceipt,
        LegacyFeedbackEventMap,
        FeedbackHistory,
        FactRelation,
        Proposal,
        ProposalTransition,
        ProposalCurrent,
        LegacyProposalMap,
    ])
}

#[derive(Clone, Debug, Deserialize, Ord, PartialOrd, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum MemoryV2ArchiveScalarV1 {
    Null,
    Integer(i64),
    RealBits(u64),
    Text(String),
    Blob(Vec<u8>),
}

#[derive(Clone, Debug, Deserialize, Ord, PartialOrd, Eq, PartialEq, Serialize)]
pub struct MemoryV2ArchiveReferenceV1 {
    family: MemoryV2ArchiveFamilyV1,
    key: BTreeMap<String, MemoryV2ArchiveScalarV1>,
}

impl MemoryV2ArchiveReferenceV1 {
    pub fn new(
        family: MemoryV2ArchiveFamilyV1,
        key: BTreeMap<String, MemoryV2ArchiveScalarV1>,
    ) -> Result<Self, MemoryV2ArchiveError> {
        validate_named_values("reference key", &key, true)?;
        Ok(Self { family, key })
    }

    pub fn family(&self) -> MemoryV2ArchiveFamilyV1 {
        self.family
    }

    pub fn key(&self) -> &BTreeMap<String, MemoryV2ArchiveScalarV1> {
        &self.key
    }
}

#[derive(Clone, Debug, Deserialize, Ord, PartialOrd, Eq, PartialEq, Serialize)]
pub struct MemoryV2ArchiveRecordV1 {
    family: MemoryV2ArchiveFamilyV1,
    key: BTreeMap<String, MemoryV2ArchiveScalarV1>,
    fields: BTreeMap<String, MemoryV2ArchiveScalarV1>,
    references: Vec<MemoryV2ArchiveReferenceV1>,
}

impl MemoryV2ArchiveRecordV1 {
    pub fn new(
        family: MemoryV2ArchiveFamilyV1,
        key: BTreeMap<String, MemoryV2ArchiveScalarV1>,
        fields: BTreeMap<String, MemoryV2ArchiveScalarV1>,
        mut references: Vec<MemoryV2ArchiveReferenceV1>,
    ) -> Result<Self, MemoryV2ArchiveError> {
        validate_named_values("record key", &key, true)?;
        validate_named_values("record fields", &fields, false)?;
        references.sort();
        references.dedup();
        Ok(Self {
            family,
            key,
            fields,
            references,
        })
    }

    pub fn family(&self) -> MemoryV2ArchiveFamilyV1 {
        self.family
    }

    pub fn key(&self) -> &BTreeMap<String, MemoryV2ArchiveScalarV1> {
        &self.key
    }

    pub fn fields(&self) -> &BTreeMap<String, MemoryV2ArchiveScalarV1> {
        &self.fields
    }

    pub fn references(&self) -> &[MemoryV2ArchiveReferenceV1] {
        &self.references
    }

    fn validate(&self) -> Result<(), MemoryV2ArchiveError> {
        validate_named_values("record key", &self.key, true)?;
        validate_named_values("record fields", &self.fields, false)?;
        for reference in &self.references {
            validate_named_values("reference key", &reference.key, true)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemoryV2OwnerArchiveV1 {
    schema: String,
    owner: FactOwnerV1,
    covered_families: BTreeSet<MemoryV2ArchiveFamilyV1>,
    records: Vec<MemoryV2ArchiveRecordV1>,
}

#[derive(Deserialize)]
struct MemoryV2OwnerArchiveWireV1 {
    schema: String,
    owner: FactOwnerV1,
    covered_families: BTreeSet<MemoryV2ArchiveFamilyV1>,
    records: Vec<MemoryV2ArchiveRecordV1>,
}

impl<'de> Deserialize<'de> for MemoryV2OwnerArchiveV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = MemoryV2OwnerArchiveWireV1::deserialize(deserializer)?;
        Self::new_with_schema(wire.schema, wire.owner, wire.covered_families, wire.records)
            .map_err(serde::de::Error::custom)
    }
}

impl MemoryV2OwnerArchiveV1 {
    pub fn new(
        owner: FactOwnerV1,
        covered_families: BTreeSet<MemoryV2ArchiveFamilyV1>,
        records: Vec<MemoryV2ArchiveRecordV1>,
    ) -> Result<Self, MemoryV2ArchiveError> {
        Self::new_with_schema(
            MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1.to_owned(),
            owner,
            covered_families,
            records,
        )
    }

    fn new_with_schema(
        schema: String,
        owner: FactOwnerV1,
        covered_families: BTreeSet<MemoryV2ArchiveFamilyV1>,
        records: Vec<MemoryV2ArchiveRecordV1>,
    ) -> Result<Self, MemoryV2ArchiveError> {
        if schema != MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1 {
            return Err(MemoryV2ArchiveError::UnsupportedSchema { schema });
        }
        owner
            .validate()
            .map_err(|error| MemoryV2ArchiveError::InvalidOwner(error.to_string()))?;
        let required = authoritative_memory_v2_archive_families();
        if covered_families != required {
            return Err(MemoryV2ArchiveError::IncompleteFamilyCoverage {
                missing: required.difference(&covered_families).copied().collect(),
                unexpected: covered_families.difference(&required).copied().collect(),
            });
        }

        let mut canonical = BTreeMap::new();
        for record in records {
            record.validate()?;
            if !covered_families.contains(&record.family) {
                return Err(MemoryV2ArchiveError::UncoveredRecordFamily {
                    family: record.family,
                });
            }
            let identity = (record.family, record.key.clone());
            if let Some(previous) = canonical.insert(identity.clone(), record.clone())
                && previous != record
            {
                return Err(MemoryV2ArchiveError::DuplicateIdentityConflict {
                    family: identity.0,
                    key: identity.1,
                });
            }
        }
        let records: Vec<_> = canonical.into_values().collect();
        let identities: BTreeSet<_> = records
            .iter()
            .map(|record| (record.family, record.key.clone()))
            .collect();
        for record in &records {
            for reference in &record.references {
                if !identities.contains(&(reference.family, reference.key.clone())) {
                    return Err(MemoryV2ArchiveError::MissingReference {
                        source_family: record.family,
                        source_key: record.key.clone(),
                        target_family: reference.family,
                        target_key: reference.key.clone(),
                    });
                }
            }
        }

        Ok(Self {
            schema,
            owner,
            covered_families,
            records,
        })
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn covered_families(&self) -> &BTreeSet<MemoryV2ArchiveFamilyV1> {
        &self.covered_families
    }

    pub fn records(&self) -> &[MemoryV2ArchiveRecordV1] {
        &self.records
    }

    pub fn digest(&self) -> Result<ManifestDigest, MemoryV2ArchiveError> {
        canonical_sha256(self).map_err(|error| MemoryV2ArchiveError::Digest(error.to_string()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryV2ArchiveConflictV1 {
    source: MemoryV2ArchiveRecordV1,
    target: MemoryV2ArchiveRecordV1,
}

impl MemoryV2ArchiveConflictV1 {
    pub fn source(&self) -> &MemoryV2ArchiveRecordV1 {
        &self.source
    }

    pub fn target(&self) -> &MemoryV2ArchiveRecordV1 {
        &self.target
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryV2OwnerMergePlanV1 {
    owner: FactOwnerV1,
    source_digest: ManifestDigest,
    target_digest: ManifestDigest,
    inserts: Vec<MemoryV2ArchiveRecordV1>,
    noops: Vec<MemoryV2ArchiveRecordV1>,
    conflicts: Vec<MemoryV2ArchiveConflictV1>,
}

impl MemoryV2OwnerMergePlanV1 {
    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn source_digest(&self) -> &ManifestDigest {
        &self.source_digest
    }

    pub fn target_digest(&self) -> &ManifestDigest {
        &self.target_digest
    }

    pub fn inserts(&self) -> &[MemoryV2ArchiveRecordV1] {
        &self.inserts
    }

    pub fn noops(&self) -> &[MemoryV2ArchiveRecordV1] {
        &self.noops
    }

    pub fn conflicts(&self) -> &[MemoryV2ArchiveConflictV1] {
        &self.conflicts
    }

    pub fn can_apply(&self) -> bool {
        self.conflicts.is_empty()
    }
}

pub fn plan_memory_v2_owner_merge(
    source: &MemoryV2OwnerArchiveV1,
    target: &MemoryV2OwnerArchiveV1,
) -> Result<MemoryV2OwnerMergePlanV1, MemoryV2ArchiveError> {
    if source.owner != target.owner {
        return Err(MemoryV2ArchiveError::OwnerMismatch);
    }
    if source.schema != target.schema {
        return Err(MemoryV2ArchiveError::SchemaMismatch);
    }

    let target_records: BTreeMap<_, _> = target
        .records
        .iter()
        .map(|record| ((record.family, record.key.clone()), record))
        .collect();
    let mut inserts = Vec::new();
    let mut noops = Vec::new();
    let mut conflicts = Vec::new();
    for record in &source.records {
        match target_records.get(&(record.family, record.key.clone())) {
            None => inserts.push(record.clone()),
            Some(existing) if *existing == record => noops.push(record.clone()),
            Some(existing) => conflicts.push(MemoryV2ArchiveConflictV1 {
                source: record.clone(),
                target: (*existing).clone(),
            }),
        }
    }
    Ok(MemoryV2OwnerMergePlanV1 {
        owner: source.owner.clone(),
        source_digest: source.digest()?,
        target_digest: target.digest()?,
        inserts,
        noops,
        conflicts,
    })
}

fn validate_named_values(
    context: &'static str,
    values: &BTreeMap<String, MemoryV2ArchiveScalarV1>,
    require_non_empty: bool,
) -> Result<(), MemoryV2ArchiveError> {
    if require_non_empty && values.is_empty() {
        return Err(MemoryV2ArchiveError::EmptyIdentity { context });
    }
    if let Some(name) = values.keys().find(|name| name.trim().is_empty()) {
        return Err(MemoryV2ArchiveError::EmptyFieldName {
            context,
            name: name.clone(),
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum MemoryV2ArchiveError {
    #[error("unsupported Memory V2 owner archive schema `{schema}`")]
    UnsupportedSchema { schema: String },
    #[error("invalid Memory V2 archive owner: {0}")]
    InvalidOwner(String),
    #[error("Memory V2 archive does not cover every authoritative family")]
    IncompleteFamilyCoverage {
        missing: Vec<MemoryV2ArchiveFamilyV1>,
        unexpected: Vec<MemoryV2ArchiveFamilyV1>,
    },
    #[error("Memory V2 archive record belongs to an uncovered family: {family:?}")]
    UncoveredRecordFamily { family: MemoryV2ArchiveFamilyV1 },
    #[error("Memory V2 archive has an incompatible duplicate {family:?} identity")]
    DuplicateIdentityConflict {
        family: MemoryV2ArchiveFamilyV1,
        key: BTreeMap<String, MemoryV2ArchiveScalarV1>,
    },
    #[error("Memory V2 archive record has a missing relationship target")]
    MissingReference {
        source_family: MemoryV2ArchiveFamilyV1,
        source_key: BTreeMap<String, MemoryV2ArchiveScalarV1>,
        target_family: MemoryV2ArchiveFamilyV1,
        target_key: BTreeMap<String, MemoryV2ArchiveScalarV1>,
    },
    #[error("{context} must not be empty")]
    EmptyIdentity { context: &'static str },
    #[error("{context} contains an empty field name")]
    EmptyFieldName { context: &'static str, name: String },
    #[error("Memory V2 owner archives have incompatible owners")]
    OwnerMismatch,
    #[error("Memory V2 owner archives have incompatible schemas")]
    SchemaMismatch,
    #[error("Memory V2 owner archive digest failed: {0}")]
    Digest(String),
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use tracedecay_domain::{FactOwnerV1, ProjectId};

    use super::*;

    fn owner() -> FactOwnerV1 {
        FactOwnerV1::Project {
            project_id: ProjectId::new("project.archive-test".to_owned()).unwrap(),
        }
    }

    fn scalar(value: &str) -> MemoryV2ArchiveScalarV1 {
        MemoryV2ArchiveScalarV1::Text(value.to_owned())
    }

    fn record(family: MemoryV2ArchiveFamilyV1, id: &str, value: &str) -> MemoryV2ArchiveRecordV1 {
        MemoryV2ArchiveRecordV1::new(
            family,
            BTreeMap::from([("id".to_owned(), scalar(id))]),
            BTreeMap::from([("value".to_owned(), scalar(value))]),
            Vec::new(),
        )
        .unwrap()
    }

    fn archive(records: Vec<MemoryV2ArchiveRecordV1>) -> MemoryV2OwnerArchiveV1 {
        MemoryV2OwnerArchiveV1::new(owner(), authoritative_memory_v2_archive_families(), records)
            .unwrap()
    }

    #[test]
    fn owner_archive_roundtrip_order_and_digest_are_deterministic() {
        let first = archive(vec![
            record(MemoryV2ArchiveFamilyV1::LineageEvent, "event.2", "two"),
            record(MemoryV2ArchiveFamilyV1::Fact, "fact.1", "one"),
        ]);
        let second = archive(vec![
            record(MemoryV2ArchiveFamilyV1::Fact, "fact.1", "one"),
            record(MemoryV2ArchiveFamilyV1::LineageEvent, "event.2", "two"),
        ]);
        assert_eq!(first, second);
        assert_eq!(first.digest().unwrap(), second.digest().unwrap());

        let encoded = serde_json::to_vec(&first).unwrap();
        let decoded: MemoryV2OwnerArchiveV1 = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, first);
        assert_eq!(decoded.digest().unwrap(), first.digest().unwrap());
    }

    #[test]
    fn archive_deserialization_fails_closed_on_unknown_schema() {
        let encoded = serde_json::to_string(&archive(Vec::new())).unwrap();
        let incompatible = encoded.replace(
            MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1,
            "tracedecay.memory-v2-owner-archive.v2",
        );
        assert!(serde_json::from_str::<MemoryV2OwnerArchiveV1>(&incompatible).is_err());
    }

    #[test]
    fn archive_requires_complete_authoritative_family_coverage() {
        let expected = BTreeSet::from([
            MemoryV2ArchiveFamilyV1::RetrievalAnchor,
            MemoryV2ArchiveFamilyV1::RetrievalAnchorAlias,
            MemoryV2ArchiveFamilyV1::RetrievalAnchorDisposition,
            MemoryV2ArchiveFamilyV1::RetrievalAnchorReverseLineage,
            MemoryV2ArchiveFamilyV1::RetrievalAnchorDerivativeTombstone,
            MemoryV2ArchiveFamilyV1::EvidenceSourceOccurrence,
            MemoryV2ArchiveFamilyV1::EvidenceOccurrenceSet,
            MemoryV2ArchiveFamilyV1::EvidenceOccurrenceSetMember,
            MemoryV2ArchiveFamilyV1::EvidenceSpan,
            MemoryV2ArchiveFamilyV1::EvidenceSpanMember,
            MemoryV2ArchiveFamilyV1::EvidenceSpanProjectionReceipt,
            MemoryV2ArchiveFamilyV1::EvidenceRetrieverContribution,
            MemoryV2ArchiveFamilyV1::EvidenceDerivedAnchor,
            MemoryV2ArchiveFamilyV1::EvidenceAssemblyReceipt,
            MemoryV2ArchiveFamilyV1::Fact,
            MemoryV2ArchiveFamilyV1::Assertion,
            MemoryV2ArchiveFamilyV1::AssertionSupersession,
            MemoryV2ArchiveFamilyV1::AssertionPayload,
            MemoryV2ArchiveFamilyV1::AssertionVector,
            MemoryV2ArchiveFamilyV1::FactEvidence,
            MemoryV2ArchiveFamilyV1::AssertionEvidence,
            MemoryV2ArchiveFamilyV1::LineageEvent,
            MemoryV2ArchiveFamilyV1::CurrentFact,
            MemoryV2ArchiveFamilyV1::LegacyFactMap,
            MemoryV2ArchiveFamilyV1::LegacyQuarantine,
            MemoryV2ArchiveFamilyV1::CompatibilityOperationReceipt,
            MemoryV2ArchiveFamilyV1::LegacyFeedbackEventMap,
            MemoryV2ArchiveFamilyV1::FeedbackHistory,
            MemoryV2ArchiveFamilyV1::FactRelation,
            MemoryV2ArchiveFamilyV1::Proposal,
            MemoryV2ArchiveFamilyV1::ProposalTransition,
            MemoryV2ArchiveFamilyV1::ProposalCurrent,
            MemoryV2ArchiveFamilyV1::LegacyProposalMap,
        ]);
        assert_eq!(authoritative_memory_v2_archive_families(), expected);
        assert!(MemoryV2OwnerArchiveV1::new(owner(), BTreeSet::new(), Vec::new()).is_err());
    }

    #[test]
    fn archive_rejects_missing_relationship_targets() {
        let reference = MemoryV2ArchiveReferenceV1::new(
            MemoryV2ArchiveFamilyV1::Fact,
            BTreeMap::from([("id".to_owned(), scalar("fact.missing"))]),
        )
        .unwrap();
        let assertion = MemoryV2ArchiveRecordV1::new(
            MemoryV2ArchiveFamilyV1::Assertion,
            BTreeMap::from([("id".to_owned(), scalar("assertion.1"))]),
            BTreeMap::new(),
            vec![reference],
        )
        .unwrap();
        assert!(
            MemoryV2OwnerArchiveV1::new(
                owner(),
                authoritative_memory_v2_archive_families(),
                vec![assertion],
            )
            .is_err()
        );
    }

    #[test]
    fn merge_planner_encodes_new_identical_and_conflicting_rows() {
        let existing = record(MemoryV2ArchiveFamilyV1::Fact, "fact.same", "same");
        let target = archive(vec![existing.clone()]);
        let source = archive(vec![
            existing,
            record(MemoryV2ArchiveFamilyV1::Fact, "fact.new", "new"),
        ]);
        let plan = plan_memory_v2_owner_merge(&source, &target).unwrap();
        assert_eq!(plan.noops().len(), 1);
        assert_eq!(plan.inserts().len(), 1);
        assert!(plan.conflicts().is_empty());

        let incompatible = archive(vec![record(
            MemoryV2ArchiveFamilyV1::Fact,
            "fact.same",
            "different",
        )]);
        let conflict = plan_memory_v2_owner_merge(&incompatible, &target).unwrap();
        assert!(conflict.inserts().is_empty());
        assert_eq!(conflict.conflicts().len(), 1);
        assert!(!conflict.can_apply());
    }

    #[test]
    fn merge_planner_rejects_same_identity_with_incompatible_relationship_history() {
        let fact = record(MemoryV2ArchiveFamilyV1::Fact, "fact.1", "same");
        let other_fact = record(MemoryV2ArchiveFamilyV1::Fact, "fact.2", "same");
        let assertion_key = BTreeMap::from([("id".to_owned(), scalar("assertion.1"))]);
        let source_assertion = MemoryV2ArchiveRecordV1::new(
            MemoryV2ArchiveFamilyV1::Assertion,
            assertion_key.clone(),
            BTreeMap::new(),
            vec![
                MemoryV2ArchiveReferenceV1::new(MemoryV2ArchiveFamilyV1::Fact, fact.key().clone())
                    .unwrap(),
            ],
        )
        .unwrap();
        let target_assertion = MemoryV2ArchiveRecordV1::new(
            MemoryV2ArchiveFamilyV1::Assertion,
            assertion_key,
            BTreeMap::new(),
            vec![
                MemoryV2ArchiveReferenceV1::new(
                    MemoryV2ArchiveFamilyV1::Fact,
                    other_fact.key().clone(),
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let source = archive(vec![fact.clone(), other_fact.clone(), source_assertion]);
        let target = archive(vec![fact, other_fact, target_assertion]);
        let plan = plan_memory_v2_owner_merge(&source, &target).unwrap();
        assert_eq!(plan.conflicts().len(), 1);
        assert_eq!(
            plan.conflicts()[0].source().family(),
            MemoryV2ArchiveFamilyV1::Assertion
        );
        assert!(!plan.can_apply());
    }

    #[test]
    fn merge_planner_fails_closed_on_scope_mismatch() {
        let profile = MemoryV2OwnerArchiveV1::new(
            FactOwnerV1::Profile,
            authoritative_memory_v2_archive_families(),
            Vec::new(),
        )
        .unwrap();
        assert!(plan_memory_v2_owner_merge(&archive(Vec::new()), &profile).is_err());
    }

    #[test]
    fn stale_legacy_map_is_retained_without_creating_a_second_fact() {
        let fact = record(MemoryV2ArchiveFamilyV1::Fact, "fact.stable", "payload");
        let fact_reference =
            MemoryV2ArchiveReferenceV1::new(MemoryV2ArchiveFamilyV1::Fact, fact.key().clone())
                .unwrap();
        let stale_map = MemoryV2ArchiveRecordV1::new(
            MemoryV2ArchiveFamilyV1::LegacyFactMap,
            BTreeMap::from([(
                "legacy_fact_id".to_owned(),
                MemoryV2ArchiveScalarV1::Integer(7),
            )]),
            BTreeMap::from([("fact_id".to_owned(), scalar("fact.stable"))]),
            vec![fact_reference],
        )
        .unwrap();
        let plan =
            plan_memory_v2_owner_merge(&archive(vec![fact, stale_map]), &archive(Vec::new()))
                .unwrap();
        assert!(plan.can_apply());
        assert_eq!(
            plan.inserts()
                .iter()
                .filter(|record| record.family() == MemoryV2ArchiveFamilyV1::Fact)
                .count(),
            1
        );
        assert!(
            plan.inserts()
                .iter()
                .any(|record| { record.family() == MemoryV2ArchiveFamilyV1::LegacyFactMap })
        );
    }
}
