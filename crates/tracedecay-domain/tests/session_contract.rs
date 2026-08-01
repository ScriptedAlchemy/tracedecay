use std::fmt::Debug;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tracedecay_domain::{
    ByteRangeV1, CanonicalObservationIdV1, CanonicalObservationRelationsV1, CompactContextBundleV1,
    CompactContextConflictV1, CompactContextLineageEdgeV1, CompactContextOmissionV1,
    CompactContextRecordV1, ContextOmissionReasonV1, CopyProofV1, EntityKind, GroupingProvenanceV1,
    HydrationStateV1, LogicalCopyRecordV1, MessageId, MessageOccurrenceIdV1,
    MessageOccurrenceRecordV1, ObservationId, ProjectionOutputOrdinalV1, RetrievalAnchorId,
    RetrievalGrainV1, SessionAuthorityClassV1, SessionContractError, SessionCursorKeyIdV1,
    SessionCursorVersionV1, SessionEvidenceMetadataV1, SessionId, SessionProjectionGenerationV1,
    SessionRefreshOperationIdV1, SessionSummaryIdV1, SessionSummaryRecordV1, SignedCursorKeyRefV1,
    SummaryPublicationMetadataV1, SummarySourceHorizonV1, TemporalAssertionIdV1,
    TemporalAssertionKindV1, TemporalAssertionRecordV1, TemporalCoverageCountsV1, TemporalModeV1,
    TemporalValidityV1, UtcMicros,
};

fn assert_json_round_trip<T>(value: T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let encoded = serde_json::to_value(&value).unwrap();
    let decoded = serde_json::from_value::<T>(encoded).unwrap();
    assert_eq!(decoded, value);
}

macro_rules! assert_json_round_trip {
    ($value:expr) => {
        assert_json_round_trip($value);
    };
}

fn observation_id() -> CanonicalObservationIdV1 {
    CanonicalObservationIdV1::new(format!("sha256:{}", "1".repeat(64))).unwrap()
}

fn anchor(value: &str) -> RetrievalAnchorId {
    RetrievalAnchorId::new(value).unwrap()
}

fn occurrence(ordinal: u32) -> MessageOccurrenceIdV1 {
    MessageOccurrenceIdV1::derive(&observation_id(), ProjectionOutputOrdinalV1::new(ordinal))
}

fn derived_grouping() -> GroupingProvenanceV1 {
    serde_json::from_value(json!({
        "kind": "derived_role_boundary",
        "projector_version": "projector.fixture"
    }))
    .unwrap()
}

fn evidence_wire(evidence_class: &str) -> Value {
    json!({
        "authority": "provider_native",
        "evidence_class": evidence_class,
        "source_anchor_id": "anchor.evidence",
        "sanitization_receipt": {
            "receipt_id": "receipt.fixture",
            "sanitizer_version": "sanitizer.fixture"
        }
    })
}

fn evidence() -> SessionEvidenceMetadataV1 {
    serde_json::from_value(evidence_wire("provider_declared")).unwrap()
}

fn summary_publication() -> SummaryPublicationMetadataV1 {
    serde_json::from_value(json!({
        "model_route": "summary.model.fixture",
        "configuration_digest": format!("sha256:{}", "3".repeat(64)),
        "sanitization_receipt": {
            "receipt_id": "receipt.fixture",
            "sanitizer_version": "sanitizer.fixture"
        }
    }))
    .unwrap()
}

fn occurrence_record_wire() -> Value {
    json!({
        "occurrence_id": occurrence(0),
        "source_observation_id": observation_id(),
        "projection_output_ordinal": 0,
        "retrieval_anchor_id": "anchor.occurrence",
        "session_id": "session.fixture",
        "thread_id": "thread.fixture",
        "thread_grouping": {"kind": "provider_native"},
        "turn_id": "turn.fixture",
        "turn_grouping": {
            "kind": "derived_role_boundary",
            "projector_version": "projector.fixture"
        },
        "message_id": "message.fixture",
        "agent_id": "agent.fixture",
        "role": "user",
        "knowledge_at": 50,
        "valid_time": {"kind": "known", "valid_at": 40},
        "evidence": evidence_wire("provider_declared")
    })
}

#[test]
fn session_occurrence_id_is_stable_and_ordinal_bound() {
    let observation_id = observation_id();
    let first = MessageOccurrenceIdV1::derive(&observation_id, ProjectionOutputOrdinalV1::new(0));
    let repeated =
        MessageOccurrenceIdV1::derive(&observation_id, ProjectionOutputOrdinalV1::new(0));
    let second = MessageOccurrenceIdV1::derive(&observation_id, ProjectionOutputOrdinalV1::new(1));

    assert_eq!(first, repeated);
    assert_ne!(first, second);
    assert_eq!(
        first.as_str(),
        "sha256:5bbe1fdde532c15044fa83cf94e10e137c964753d5af2c39cf3f67b6c21c3c85"
    );
}

#[test]
fn temporal_modes_round_trip_and_unknown_valid_time_is_not_representative_as_of() {
    let cutoff = UtcMicros(50);
    let mode = TemporalModeV1::AsOf { cutoff };

    assert_eq!(
        serde_json::to_value(mode).unwrap(),
        json!({"kind": "as_of", "cutoff": 50})
    );
    assert!(
        !TemporalValidityV1::Unknown
            .is_representative_at(UtcMicros(40), TemporalModeV1::AsOf { cutoff },)
    );
    assert!(
        TemporalValidityV1::Unknown.is_representative_at(UtcMicros(40), TemporalModeV1::Forensic,)
    );
    assert!(
        TemporalValidityV1::Known {
            valid_at: UtcMicros(45)
        }
        .is_representative_at(UtcMicros(40), TemporalModeV1::AsOf { cutoff })
    );
    assert!(
        !TemporalValidityV1::Known {
            valid_at: UtcMicros(55)
        }
        .is_representative_at(UtcMicros(40), TemporalModeV1::AsOf { cutoff })
    );
    assert!(
        !TemporalValidityV1::Known {
            valid_at: UtcMicros(45)
        }
        .is_representative_at(UtcMicros(55), TemporalModeV1::AsOf { cutoff })
    );
}

#[test]
fn exact_byte_ranges_are_canonical_half_open_domain_values() {
    let range = ByteRangeV1::new(3, 11).expect("ordered non-empty byte range");
    assert_eq!((range.start(), range.end()), (3, 11));
    assert_json_round_trip!(range);
    assert_eq!(
        ByteRangeV1::new(3, 3),
        Err(SessionContractError::InvalidByteRange)
    );
    assert_eq!(
        ByteRangeV1::new(4, 3),
        Err(SessionContractError::InvalidByteRange)
    );
    assert!(
        serde_json::from_value::<ByteRangeV1>(json!({
            "start": 3,
            "end": 11,
            "inclusive": true
        }))
        .is_err()
    );
}

#[test]
fn exact_byte_range_deserialization_rejects_invalid_domain_values() {
    for invalid_range in [json!({"start": 3, "end": 3}), json!({"start": 4, "end": 3})] {
        let error = serde_json::from_value::<ByteRangeV1>(invalid_range)
            .expect_err("deserialization must enforce the byte-range invariant");
        assert!(
            error
                .to_string()
                .contains("a byte range must be non-empty and ordered"),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn temporal_values_round_trip_every_variant_and_reject_unknown_variants() {
    for mode in [
        TemporalModeV1::Current,
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(50),
        },
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
    ] {
        assert_json_round_trip!(mode);
    }
    for validity in [
        TemporalValidityV1::Known {
            valid_at: UtcMicros(40),
        },
        TemporalValidityV1::Unknown,
    ] {
        assert_json_round_trip!(validity);
    }
    assert!(serde_json::from_value::<TemporalModeV1>(json!({"kind": "future"})).is_err());
    assert!(serde_json::from_value::<TemporalValidityV1>(json!({"kind": "future"})).is_err());
}

#[test]
fn copy_proofs_and_copy_records_round_trip_and_reject_invalid_links() {
    let source = occurrence(0);
    let target = occurrence(1);
    let proofs = [
        CopyProofV1::ProviderLinkage {
            source_occurrence_id: source.clone(),
            provider_record_id: ObservationId::new("provider.message.1").unwrap(),
        },
        CopyProofV1::ParentMessageLinkage {
            source_occurrence_id: source.clone(),
            parent_message_id: MessageId::new("message.parent.1").unwrap(),
        },
        CopyProofV1::ExplicitAnchorAssertion {
            source_occurrence_id: source.clone(),
            assertion_anchor_id: anchor("anchor.copy.proof"),
        },
    ];
    for proof in proofs {
        assert_eq!(proof.source_occurrence_id(), &source);
        assert_json_round_trip!(proof.clone());
        let copy = LogicalCopyRecordV1 {
            occurrence_id: target.clone(),
            copied_from_occurrence_id: source.clone(),
            proof,
            knowledge_at: UtcMicros(50),
            valid_time: TemporalValidityV1::Unknown,
        };
        copy.validate().unwrap();
        assert_json_round_trip!(copy);
    }
    assert!(
        serde_json::from_value::<CopyProofV1>(json!({
            "kind": "content_hash",
            "source_occurrence_id": source.clone(),
            "content_hash": format!("sha256:{}", "2".repeat(64))
        }))
        .is_err()
    );

    let copy = LogicalCopyRecordV1 {
        occurrence_id: target.clone(),
        copied_from_occurrence_id: source.clone(),
        proof: CopyProofV1::ProviderLinkage {
            source_occurrence_id: source.clone(),
            provider_record_id: ObservationId::new("provider.message.1").unwrap(),
        },
        knowledge_at: UtcMicros(50),
        valid_time: TemporalValidityV1::Unknown,
    };
    copy.validate().unwrap();
    assert_json_round_trip!(copy.clone());

    let self_copy = LogicalCopyRecordV1 {
        occurrence_id: target.clone(),
        copied_from_occurrence_id: target.clone(),
        proof: CopyProofV1::ProviderLinkage {
            source_occurrence_id: target.clone(),
            provider_record_id: ObservationId::new("provider.message.1").unwrap(),
        },
        knowledge_at: UtcMicros(50),
        valid_time: TemporalValidityV1::Unknown,
    };
    assert_eq!(
        self_copy.validate(),
        Err(SessionContractError::CopySelfReference)
    );
    let mut self_copy = serde_json::to_value(&copy).unwrap();
    self_copy["copied_from_occurrence_id"] = json!(target);
    assert!(serde_json::from_value::<LogicalCopyRecordV1>(self_copy).is_err());

    let mismatched_copy = LogicalCopyRecordV1 {
        occurrence_id: occurrence(1),
        copied_from_occurrence_id: source,
        proof: CopyProofV1::ProviderLinkage {
            source_occurrence_id: occurrence(2),
            provider_record_id: ObservationId::new("provider.message.1").unwrap(),
        },
        knowledge_at: UtcMicros(50),
        valid_time: TemporalValidityV1::Unknown,
    };
    assert_eq!(
        mismatched_copy.validate(),
        Err(SessionContractError::CopyProofSourceMismatch)
    );
    let mut mismatched_proof = serde_json::to_value(copy).unwrap();
    mismatched_proof["proof"]["source_occurrence_id"] = json!(occurrence(2));
    assert!(serde_json::from_value::<LogicalCopyRecordV1>(mismatched_proof).is_err());

    let legacy = serde_json::from_value::<LogicalCopyRecordV1>(json!({
        "occurrence_id": occurrence(1),
        "copied_from_occurrence_id": occurrence(0),
        "proof": {
            "kind": "provider_linkage",
            "source_occurrence_id": occurrence(0),
            "provider_record_id": "provider.message.1"
        }
    }))
    .unwrap();
    assert_eq!(legacy.knowledge_at, UtcMicros(0));
    assert_eq!(legacy.valid_time, TemporalValidityV1::Unknown);
}

#[test]
fn canonical_relations_expose_thread_turn_and_parent_message() {
    let thread = ObservationId::new("thread.native.1").unwrap();
    let turn = ObservationId::new("turn.native.1").unwrap();
    let parent = ObservationId::new("message.native.parent").unwrap();
    let relations =
        CanonicalObservationRelationsV1::new(SessionId::new("session.fixture").unwrap())
            .with_thread_id(thread.clone())
            .with_turn_id(turn.clone())
            .with_parent_message_id(parent.clone());

    assert_eq!(relations.thread_id(), Some(&thread));
    assert_eq!(relations.turn_id(), Some(&turn));
    assert_eq!(relations.parent_message_id(), Some(&parent));
}

#[test]
fn summaries_canonicalize_sources_and_reject_self_predecessors() {
    let canonical = SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new("summary.fixture").unwrap(),
        SessionId::new("session.fixture").unwrap(),
        anchor("anchor.summary"),
        vec![anchor("anchor.source.a"), anchor("anchor.source.b")],
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(50),
            valid_through: Some(UtcMicros(40)),
        },
        UtcMicros(60),
    )
    .unwrap();
    let reordered = SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new("summary.fixture").unwrap(),
        SessionId::new("session.fixture").unwrap(),
        anchor("anchor.summary"),
        vec![anchor("anchor.source.b"), anchor("anchor.source.a")],
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(50),
            valid_through: Some(UtcMicros(40)),
        },
        UtcMicros(60),
    )
    .unwrap();
    assert_eq!(canonical.source_anchors(), reordered.source_anchors());
    assert_eq!(
        serde_json::to_value(&canonical).unwrap(),
        serde_json::to_value(&reordered).unwrap()
    );
    assert_json_round_trip!(canonical.clone());
    assert_json_round_trip!(SummarySourceHorizonV1 {
        knowledge_through: UtcMicros(50),
        valid_through: None,
    });
    assert_json_round_trip!(SummarySourceHorizonV1 {
        knowledge_through: UtcMicros(50),
        valid_through: Some(UtcMicros(50)),
    });

    let empty = SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new("summary.empty").unwrap(),
        SessionId::new("session.fixture").unwrap(),
        anchor("anchor.summary.empty"),
        vec![],
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(50),
            valid_through: None,
        },
        UtcMicros(60),
    );
    assert_eq!(empty, Err(SessionContractError::SummarySourcesRequired));

    let duplicate = SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new("summary.duplicate").unwrap(),
        SessionId::new("session.fixture").unwrap(),
        anchor("anchor.summary.duplicate"),
        vec![anchor("anchor.same"), anchor("anchor.same")],
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(50),
            valid_through: None,
        },
        UtcMicros(60),
    );
    assert_eq!(duplicate, Err(SessionContractError::DuplicateSummarySource));

    let invalid_horizon = SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new("summary.invalid-horizon").unwrap(),
        SessionId::new("session.fixture").unwrap(),
        anchor("anchor.summary.invalid-horizon"),
        vec![anchor("anchor.source.invalid-horizon")],
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(50),
            valid_through: None,
        },
        UtcMicros(49),
    );
    assert_eq!(
        invalid_horizon,
        Err(SessionContractError::InvalidSummaryHorizon)
    );
    let future_effective_horizon = SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new("summary.future-effective-horizon").unwrap(),
        SessionId::new("session.fixture").unwrap(),
        anchor("anchor.summary.future-effective-horizon"),
        vec![anchor("anchor.source.future-effective-horizon")],
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(50),
            valid_through: Some(UtcMicros(60)),
        },
        UtcMicros(70),
    )
    .expect("valid time may extend beyond knowledge time");
    assert_json_round_trip!(future_effective_horizon);
    assert!(
        serde_json::from_value::<SummarySourceHorizonV1>(json!({
            "knowledge_through": 50
        }))
        .is_err()
    );

    assert_eq!(
        canonical
            .clone()
            .with_predecessor(canonical.summary_id().clone()),
        Err(SessionContractError::SummarySelfPredecessor)
    );
    let predecessor = canonical
        .clone()
        .with_predecessor(SessionSummaryIdV1::new("summary.predecessor").unwrap())
        .unwrap();
    assert_json_round_trip!(predecessor);
    assert_json_round_trip!(
        canonical
            .clone()
            .with_publication(summary_publication())
            .unwrap()
    );

    let mut self_predecessor = serde_json::to_value(canonical).unwrap();
    self_predecessor["predecessor_summary_id"] = json!("summary.fixture");
    assert!(serde_json::from_value::<SessionSummaryRecordV1>(self_predecessor).is_err());
}

#[test]
fn typed_ids_and_signed_cursors_round_trip_and_reject_invalid_values() {
    assert_json_round_trip!(SessionSummaryIdV1::new("summary.fixture").unwrap());
    assert_json_round_trip!(TemporalAssertionIdV1::new("assertion.fixture").unwrap());
    assert_json_round_trip!(SessionRefreshOperationIdV1::new("refresh.fixture").unwrap());
    assert_json_round_trip!(SessionProjectionGenerationV1::new(1).unwrap());
    assert_json_round_trip!(SessionCursorKeyIdV1::new("cursor.key.fixture").unwrap());

    let signed_cursor = SignedCursorKeyRefV1 {
        key_id: SessionCursorKeyIdV1::new("cursor.key.fixture").unwrap(),
        version: SessionCursorVersionV1::new(1).unwrap(),
    };
    assert_json_round_trip!(signed_cursor);
    assert!(serde_json::from_value::<SessionProjectionGenerationV1>(json!(0)).is_err());
    assert!(serde_json::from_value::<SessionCursorVersionV1>(json!(0)).is_err());
    assert_eq!(
        SessionProjectionGenerationV1::new(0),
        Err(SessionContractError::ZeroValue {
            field: "session projection generation"
        })
    );
    assert_eq!(
        SessionCursorVersionV1::new(0),
        Err(SessionContractError::ZeroValue {
            field: "session cursor version"
        })
    );

    macro_rules! assert_canonical_string_id {
        ($type:ty, $field:literal) => {
            for invalid in [
                String::new(),
                " leading".to_owned(),
                "trailing ".to_owned(),
                "control\ncharacter".to_owned(),
                "x".repeat(513),
            ] {
                assert_eq!(
                    <$type>::new(invalid),
                    Err(SessionContractError::InvalidIdentity { field: $field })
                );
            }
        };
    }
    assert_canonical_string_id!(SessionSummaryIdV1, "SessionSummaryIdV1");
    assert_canonical_string_id!(TemporalAssertionIdV1, "TemporalAssertionIdV1");
    assert_canonical_string_id!(SessionRefreshOperationIdV1, "SessionRefreshOperationIdV1");
    assert_canonical_string_id!(SessionCursorKeyIdV1, "SessionCursorKeyIdV1");

    for invalid in [
        "sha256:".to_owned(),
        format!("sha256:{}", "A".repeat(64)),
        format!("sha256:{}", "0".repeat(63)),
        format!("sha256:{}", "g".repeat(64)),
        format!("sha512:{}", "0".repeat(64)),
    ] {
        assert_eq!(
            MessageOccurrenceIdV1::new(invalid),
            Err(SessionContractError::InvalidIdentity {
                field: "MessageOccurrenceIdV1"
            })
        );
    }
}

/// `as_str` is what callers log, key, and route on, so it must be the same
/// string the wire carries. Sweeping `ALL` keeps a new variant covered without
/// a test edit.
macro_rules! assert_as_str_is_the_wire_value_for_all_variants {
    ($type:ty) => {
        for variant in <$type>::ALL {
            assert_json_round_trip!(variant);
            assert_eq!(serde_json::to_value(variant).unwrap(), variant.as_str());
        }
    };
}

#[test]
fn enum_as_str_matches_serde_for_every_variant() {
    assert_as_str_is_the_wire_value_for_all_variants!(RetrievalGrainV1);
    assert_as_str_is_the_wire_value_for_all_variants!(SessionAuthorityClassV1);
    assert_as_str_is_the_wire_value_for_all_variants!(TemporalAssertionKindV1);

    // These two carry data, so they serialize as tagged objects and `as_str`
    // names the tag rather than the whole value.
    for mode in [
        TemporalModeV1::Current,
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(50),
        },
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
    ] {
        assert_eq!(serde_json::to_value(mode).unwrap()["kind"], mode.as_str());
    }
    for grouping in [GroupingProvenanceV1::ProviderNative, derived_grouping()] {
        assert_json_round_trip!(grouping);
    }

    assert!(serde_json::from_value::<RetrievalGrainV1>(json!("paragraph")).is_err());
    assert!(serde_json::from_value::<SessionAuthorityClassV1>(json!("untrusted")).is_err());
    assert!(serde_json::from_value::<TemporalAssertionKindV1>(json!("merges")).is_err());
    assert!(serde_json::from_value::<GroupingProvenanceV1>(json!({"kind": "inferred"})).is_err());
}

#[test]
fn evidence_and_assertion_records_round_trip_and_reject_invalid_anchors() {
    for evidence_class in [
        "heuristic",
        "inferred",
        "derived_exact",
        "user_declared",
        "provider_declared",
        "observed",
    ] {
        let metadata: SessionEvidenceMetadataV1 =
            serde_json::from_value(evidence_wire(evidence_class)).unwrap();
        metadata.validate().unwrap();
        assert_json_round_trip!(metadata);
    }

    let metadata = evidence();
    metadata.validate().unwrap();
    assert_json_round_trip!(metadata.clone());

    let mut invalid_metadata = serde_json::to_value(&metadata).unwrap();
    invalid_metadata["source_anchor_id"] = json!(" ");
    assert!(serde_json::from_value::<SessionEvidenceMetadataV1>(invalid_metadata).is_err());

    for kind in [
        TemporalAssertionKindV1::Corrects,
        TemporalAssertionKindV1::Supersedes,
        TemporalAssertionKindV1::Contradicts,
        TemporalAssertionKindV1::Supports,
    ] {
        let assertion = TemporalAssertionRecordV1 {
            assertion_id: TemporalAssertionIdV1::new("assertion.fixture").unwrap(),
            kind,
            subject_anchor_id: anchor("anchor.assertion.subject"),
            object_anchor_id: anchor("anchor.assertion.object"),
            knowledge_at: UtcMicros(50),
            valid_time: TemporalValidityV1::Known {
                valid_at: UtcMicros(40),
            },
            evidence: metadata.clone(),
        };
        assertion.validate().unwrap();
        assert_json_round_trip!(assertion.clone());

        let self_assertion = TemporalAssertionRecordV1 {
            object_anchor_id: assertion.subject_anchor_id.clone(),
            ..assertion.clone()
        };
        assert_eq!(
            self_assertion.validate(),
            Err(SessionContractError::AssertionSelfReference)
        );
        let mut self_assertion = serde_json::to_value(assertion).unwrap();
        self_assertion["object_anchor_id"] = json!("anchor.assertion.subject");
        assert!(serde_json::from_value::<TemporalAssertionRecordV1>(self_assertion).is_err());
    }
}

#[test]
fn occurrence_records_round_trip_with_independent_grouping_and_reject_orphans() {
    let wire = occurrence_record_wire();
    let record: MessageOccurrenceRecordV1 = serde_json::from_value(wire.clone()).unwrap();
    record.validate().unwrap();
    assert_eq!(serde_json::to_value(&record).unwrap(), wire);
    assert_json_round_trip!(record.clone());

    let mut invalid_occurrence_record = record.clone();
    invalid_occurrence_record.occurrence_id = occurrence(1);
    assert_eq!(
        invalid_occurrence_record.validate(),
        Err(SessionContractError::OccurrenceIdentityMismatch)
    );

    let mut invalid_occurrence = occurrence_record_wire();
    invalid_occurrence["occurrence_id"] = json!(occurrence(1));
    assert!(serde_json::from_value::<MessageOccurrenceRecordV1>(invalid_occurrence).is_err());

    for field in ["thread_id", "turn_id"] {
        let mut orphaned_provenance = occurrence_record_wire();
        orphaned_provenance[field] = Value::Null;
        assert!(serde_json::from_value::<MessageOccurrenceRecordV1>(orphaned_provenance).is_err());
    }
    for field in ["thread_grouping", "turn_grouping"] {
        let mut unprovenanced_group = occurrence_record_wire();
        unprovenanced_group[field] = Value::Null;
        assert!(serde_json::from_value::<MessageOccurrenceRecordV1>(unprovenanced_group).is_err());
    }

    let mut orphaned_thread_grouping = record.clone();
    orphaned_thread_grouping.thread_id = None;
    assert_eq!(
        orphaned_thread_grouping.validate(),
        Err(SessionContractError::GroupingProvenanceWithoutId { group: "thread" })
    );
    let mut unprovenanced_turn = record;
    unprovenanced_turn.turn_grouping = None;
    assert_eq!(
        unprovenanced_turn.validate(),
        Err(SessionContractError::GroupingIdWithoutProvenance { group: "turn" })
    );

    let mut ungrouped_unknown = occurrence_record_wire();
    ungrouped_unknown["thread_id"] = Value::Null;
    ungrouped_unknown["thread_grouping"] = Value::Null;
    ungrouped_unknown["turn_id"] = Value::Null;
    ungrouped_unknown["turn_grouping"] = Value::Null;
    ungrouped_unknown["valid_time"] = json!({"kind": "unknown"});
    let record: MessageOccurrenceRecordV1 =
        serde_json::from_value(ungrouped_unknown.clone()).unwrap();
    record.validate().unwrap();
    assert_eq!(serde_json::to_value(record).unwrap(), ungrouped_unknown);
}

#[test]
fn hydration_and_omission_values_round_trip_every_variant_and_reject_unknown_values() {
    for state in [
        HydrationStateV1::Available,
        HydrationStateV1::RetainedButUnavailable,
        HydrationStateV1::Redacted,
        HydrationStateV1::Deleted,
        HydrationStateV1::RetentionExpired,
        HydrationStateV1::Unauthorized,
        HydrationStateV1::Locked,
        HydrationStateV1::UnverifiableLegacy,
    ] {
        assert_json_round_trip!(state);
        assert_eq!(serde_json::to_value(state).unwrap(), state.as_str());
    }
    for reason in [
        ContextOmissionReasonV1::ByteBudget,
        ContextOmissionReasonV1::TokenBudget,
        ContextOmissionReasonV1::Unauthorized,
        ContextOmissionReasonV1::Redacted,
        ContextOmissionReasonV1::Deleted,
        ContextOmissionReasonV1::RetentionExpired,
        ContextOmissionReasonV1::Locked,
        ContextOmissionReasonV1::Unavailable,
        ContextOmissionReasonV1::SummaryHorizonMismatch,
        ContextOmissionReasonV1::DuplicateRepresentative,
    ] {
        assert_json_round_trip!(CompactContextOmissionV1 {
            anchor_id: Some(anchor("anchor.omission")),
            reason,
        });
        assert_eq!(serde_json::to_value(reason).unwrap(), reason.as_str());
    }
    assert_json_round_trip!(CompactContextOmissionV1 {
        anchor_id: None,
        reason: ContextOmissionReasonV1::ByteBudget,
    });

    assert!(serde_json::from_value::<HydrationStateV1>(json!("incomplete")).is_err());
    assert!(serde_json::from_value::<ContextOmissionReasonV1>(json!("stale")).is_err());
    assert!(
        serde_json::from_value::<CompactContextOmissionV1>(json!({
            "anchor_id": " ",
            "reason": "byte_budget"
        }))
        .is_err()
    );
}

#[test]
fn compact_context_recomputes_bytes_and_validates_omission_anchors() {
    let first = CompactContextRecordV1 {
        anchor_id: anchor("anchor.context.first"),
        grain: RetrievalGrainV1::Occurrence,
        hydration: HydrationStateV1::Available,
        encoded_bytes: 3,
    };
    let second = CompactContextRecordV1 {
        anchor_id: anchor("anchor.context.second"),
        grain: RetrievalGrainV1::Summary,
        hydration: HydrationStateV1::RetainedButUnavailable,
        encoded_bytes: 5,
    };
    assert_json_round_trip!(first.clone());
    assert_json_round_trip!(second.clone());

    let bundle = CompactContextBundleV1 {
        records: vec![first.clone(), second],
        omissions: vec![CompactContextOmissionV1 {
            anchor_id: Some(anchor("anchor.context.omitted")),
            reason: ContextOmissionReasonV1::TokenBudget,
        }],
        continuation_anchors: vec![anchor("anchor.context.continuation")],
        coverage: TemporalCoverageCountsV1 {
            visible: 1,
            hidden: 2,
            unknown: 3,
            redacted: 4,
        },
        conflicts: vec![CompactContextConflictV1 {
            anchor_id: anchor("anchor.context.first"),
            supporting_anchor_ids: [anchor("anchor.context.support")].into_iter().collect(),
        }],
        lineage: vec![CompactContextLineageEdgeV1 {
            kind: TemporalAssertionKindV1::Corrects,
            subject_anchor_id: anchor("anchor.context.first"),
            object_anchor_id: anchor("anchor.context.predecessor"),
            knowledge_at: UtcMicros(42),
            authority: SessionAuthorityClassV1::CanonicalObservation,
            authorized: true,
            supporting_anchor_ids: [anchor("anchor.context.support")].into_iter().collect(),
        }],
        encoded_bytes: 8,
    };
    bundle.validate().unwrap();
    assert_json_round_trip!(bundle.clone());

    let mut incorrect_total = bundle.clone();
    incorrect_total.encoded_bytes = 9;
    assert_eq!(
        incorrect_total.validate(),
        Err(SessionContractError::CompactContextEncodedBytesMismatch)
    );
    assert!(
        serde_json::from_value::<CompactContextBundleV1>(
            serde_json::to_value(incorrect_total).unwrap()
        )
        .is_err()
    );

    let overflow = CompactContextBundleV1 {
        records: vec![
            CompactContextRecordV1 {
                anchor_id: anchor("anchor.context.overflow.first"),
                grain: RetrievalGrainV1::Occurrence,
                hydration: HydrationStateV1::Available,
                encoded_bytes: u64::MAX,
            },
            CompactContextRecordV1 {
                anchor_id: anchor("anchor.context.overflow.second"),
                grain: RetrievalGrainV1::Occurrence,
                hydration: HydrationStateV1::Available,
                encoded_bytes: 1,
            },
        ],
        omissions: vec![],
        continuation_anchors: vec![],
        coverage: TemporalCoverageCountsV1::default(),
        conflicts: vec![],
        lineage: vec![],
        encoded_bytes: u64::MAX,
    };
    assert_eq!(
        overflow.validate(),
        Err(SessionContractError::CompactContextEncodedBytesOverflow)
    );
    assert!(
        serde_json::from_value::<CompactContextBundleV1>(serde_json::to_value(overflow).unwrap())
            .is_err()
    );

    let duplicate_omission = CompactContextBundleV1 {
        records: vec![first.clone()],
        omissions: vec![CompactContextOmissionV1 {
            anchor_id: Some(anchor("anchor.context.first")),
            reason: ContextOmissionReasonV1::DuplicateRepresentative,
        }],
        continuation_anchors: vec![],
        coverage: TemporalCoverageCountsV1::default(),
        conflicts: vec![],
        lineage: vec![],
        encoded_bytes: 3,
    };
    assert!(duplicate_omission.validate().is_err());
    assert!(
        serde_json::from_value::<CompactContextBundleV1>(
            serde_json::to_value(duplicate_omission).unwrap()
        )
        .is_err()
    );

    let duplicate_record = CompactContextBundleV1 {
        records: vec![first.clone(), first.clone()],
        encoded_bytes: 6,
        ..CompactContextBundleV1::default()
    };
    assert_eq!(
        duplicate_record.validate(),
        Err(SessionContractError::DuplicateContextAnchor)
    );

    let duplicate_continuation = CompactContextBundleV1 {
        continuation_anchors: vec![
            anchor("anchor.context.continuation"),
            anchor("anchor.context.continuation"),
        ],
        ..CompactContextBundleV1::default()
    };
    assert_eq!(
        duplicate_continuation.validate(),
        Err(SessionContractError::DuplicateContextAnchor)
    );

    let record_and_continuation = CompactContextBundleV1 {
        records: vec![first],
        continuation_anchors: vec![anchor("anchor.context.first")],
        encoded_bytes: 3,
        ..CompactContextBundleV1::default()
    };
    assert_eq!(
        record_and_continuation.validate(),
        Err(SessionContractError::DuplicateContextAnchor)
    );
}

#[test]
fn compact_context_lineage_rejects_self_edges_like_assertion_records() {
    let edge = CompactContextLineageEdgeV1 {
        kind: TemporalAssertionKindV1::Supersedes,
        subject_anchor_id: anchor("anchor.lineage.same"),
        object_anchor_id: anchor("anchor.lineage.same"),
        knowledge_at: UtcMicros(50),
        authority: SessionAuthorityClassV1::ExplicitAnchorAssertion,
        authorized: true,
        supporting_anchor_ids: [anchor("anchor.lineage.proof")].into_iter().collect(),
    };

    assert_eq!(
        edge.validate(),
        Err(SessionContractError::AssertionSelfReference)
    );
    let bundle = CompactContextBundleV1 {
        lineage: vec![edge],
        ..CompactContextBundleV1::default()
    };
    assert_eq!(
        bundle.validate(),
        Err(SessionContractError::AssertionSelfReference)
    );
    assert!(
        serde_json::from_value::<CompactContextBundleV1>(serde_json::to_value(bundle).unwrap())
            .is_err()
    );
}

#[test]
fn session_wire_records_reject_unknown_fields() {
    fn assert_unknown_field_rejected<T>(mut value: Value)
    where
        T: DeserializeOwned,
    {
        value["unexpected"] = json!(true);
        assert!(
            serde_json::from_value::<T>(value).is_err(),
            "{} accepted an unknown field",
            std::any::type_name::<T>()
        );
    }

    assert_unknown_field_rejected::<SignedCursorKeyRefV1>(json!({
        "key_id": "cursor.key.fixture",
        "version": 1
    }));
    assert_unknown_field_rejected::<TemporalModeV1>(json!({"kind": "current"}));
    assert_unknown_field_rejected::<TemporalModeV1>(json!({
        "kind": "as_of",
        "cutoff": 50
    }));
    assert_unknown_field_rejected::<TemporalValidityV1>(json!({"kind": "unknown"}));
    assert_unknown_field_rejected::<TemporalValidityV1>(json!({
        "kind": "known",
        "valid_at": 50
    }));
    assert_unknown_field_rejected::<GroupingProvenanceV1>(json!({"kind": "provider_native"}));
    assert_unknown_field_rejected::<GroupingProvenanceV1>(json!({
        "kind": "derived_role_boundary",
        "projector_version": "projector.fixture"
    }));
    assert_unknown_field_rejected::<SessionEvidenceMetadataV1>(evidence_wire("observed"));
    assert_unknown_field_rejected::<MessageOccurrenceRecordV1>(occurrence_record_wire());
    assert_unknown_field_rejected::<CopyProofV1>(
        serde_json::to_value(CopyProofV1::ProviderLinkage {
            source_occurrence_id: occurrence(0),
            provider_record_id: ObservationId::new("provider.message.1").unwrap(),
        })
        .unwrap(),
    );
    assert_unknown_field_rejected::<LogicalCopyRecordV1>(json!({
        "occurrence_id": occurrence(1),
        "copied_from_occurrence_id": occurrence(0),
        "proof": {
            "kind": "provider_linkage",
            "source_occurrence_id": occurrence(0),
            "provider_record_id": "provider.message.1"
        }
    }));
    assert_unknown_field_rejected::<TemporalAssertionRecordV1>(json!({
        "assertion_id": "assertion.fixture",
        "kind": "supports",
        "subject_anchor_id": "anchor.subject",
        "object_anchor_id": "anchor.object",
        "knowledge_at": 50,
        "valid_time": {"kind": "unknown"},
        "evidence": evidence_wire("observed")
    }));
    assert_unknown_field_rejected::<SummarySourceHorizonV1>(json!({
        "knowledge_through": 50,
        "valid_through": 40
    }));
    assert_unknown_field_rejected::<SummaryPublicationMetadataV1>(
        serde_json::to_value(summary_publication()).unwrap(),
    );

    let summary = SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new("summary.fixture").unwrap(),
        SessionId::new("session.fixture").unwrap(),
        anchor("anchor.summary"),
        vec![anchor("anchor.source")],
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(50),
            valid_through: None,
        },
        UtcMicros(60),
    )
    .unwrap();
    assert_unknown_field_rejected::<SessionSummaryRecordV1>(serde_json::to_value(summary).unwrap());
    assert_unknown_field_rejected::<CompactContextRecordV1>(json!({
        "anchor_id": "anchor.context",
        "grain": "occurrence",
        "hydration": "available",
        "encoded_bytes": 1
    }));
    assert_unknown_field_rejected::<CompactContextOmissionV1>(json!({
        "anchor_id": null,
        "reason": "byte_budget"
    }));
    assert_unknown_field_rejected::<CompactContextConflictV1>(json!({
        "anchor_id": "anchor.conflict",
        "supporting_anchor_ids": []
    }));
    assert_unknown_field_rejected::<CompactContextLineageEdgeV1>(json!({
        "kind": "supports",
        "subject_anchor_id": "anchor.subject",
        "object_anchor_id": "anchor.object",
        "knowledge_at": 50,
        "authority": "provider_native",
        "authorized": true,
        "supporting_anchor_ids": []
    }));
    assert_unknown_field_rejected::<CompactContextBundleV1>(json!({
        "records": [],
        "omissions": [],
        "continuation_anchors": [],
        "coverage": {"visible": 0, "hidden": 0, "unknown": 0, "redacted": 0},
        "conflicts": [],
        "lineage": [],
        "encoded_bytes": 0
    }));
    assert_unknown_field_rejected::<TemporalCoverageCountsV1>(json!({
        "visible": 0,
        "hidden": 0,
        "unknown": 0,
        "redacted": 0
    }));
}

#[test]
fn compact_context_temporal_frames_are_required_in_memory_and_default_on_legacy_wire() {
    let legacy = json!({
        "records": [],
        "omissions": [],
        "continuation_anchors": [],
        "encoded_bytes": 0
    });
    let decoded: CompactContextBundleV1 = serde_json::from_value(legacy).unwrap();

    assert_eq!(decoded.coverage, TemporalCoverageCountsV1::default());
    assert!(decoded.conflicts.is_empty());
    assert!(decoded.lineage.is_empty());

    let encoded = serde_json::to_value(decoded).unwrap();
    assert_eq!(encoded["coverage"]["visible"], 0);
    assert_eq!(encoded["conflicts"], json!([]));
    assert_eq!(encoded["lineage"], json!([]));
}

#[test]
fn coverage_and_anchor_entity_kinds_have_stable_wire_values() {
    let coverage = TemporalCoverageCountsV1 {
        visible: 3,
        hidden: 2,
        unknown: 1,
        redacted: 4,
    };
    assert_eq!(coverage.total(), Some(10));
    assert!(coverage.has_withheld_or_unknown());
    assert_json_round_trip!(coverage);

    let kinds = [
        (EntityKind::Thread, "thread"),
        (EntityKind::Turn, "turn"),
        (EntityKind::Agent, "agent"),
        (EntityKind::MessageOccurrence, "message_occurrence"),
        (EntityKind::SessionSummary, "session_summary"),
        (EntityKind::EvidenceSpan, "evidence_span"),
        (EntityKind::EvidenceBurst, "evidence_burst"),
    ];
    for (kind, expected) in kinds {
        let encoded = serde_json::to_value(&kind).unwrap();
        assert_eq!(encoded, json!(expected));
        assert_eq!(serde_json::from_value::<EntityKind>(encoded).unwrap(), kind);
    }
}

#[test]
fn derived_evidence_ids_and_manifests_are_stable_and_reject_malformed_inputs() {
    use sha2::{Digest, Sha256};
    use tracedecay_domain::{
        DerivedEvidenceKindV1, DerivedEvidenceOccurrenceRefV1, MessageId, MessageOccurrenceIdV1,
        RetrievalAnchorId, SESSION_DERIVED_SPAN_MAX_MEMBERS_V1, SessionDerivedEvidencePolicyV1,
        SessionId, ThreadId, UtcMicros, derive_session_evidence_from_occurrences,
    };

    fn sha_id(label: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(label.as_bytes());
        let digest = hasher.finalize();
        let mut encoded = String::with_capacity(71);
        encoded.push_str("sha256:");
        for byte in digest {
            use std::fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").unwrap();
        }
        encoded
    }

    let session_id = SessionId::new("session.derived.contract").unwrap();
    let policy = SessionDerivedEvidencePolicyV1 {
        span_max_members: SESSION_DERIVED_SPAN_MAX_MEMBERS_V1,
    };
    let occurrences = (0..3)
        .map(|index| DerivedEvidenceOccurrenceRefV1 {
            occurrence_id: MessageOccurrenceIdV1::new(sha_id(&format!("occurrence-{index}")))
                .unwrap(),
            retrieval_anchor_id: RetrievalAnchorId::new(sha_id(&format!("anchor-{index}")))
                .unwrap(),
            thread_id: Some(ThreadId::new("thread.derived").unwrap()),
            message_id: Some(MessageId::new(format!("message.derived.{index}")).unwrap()),
            knowledge_at: UtcMicros(index),
            observation_sequence: index as u64,
            projection_output_ordinal: 0,
        })
        .collect::<Vec<_>>();

    let first =
        derive_session_evidence_from_occurrences(&session_id, &occurrences, &policy).unwrap();
    let second =
        derive_session_evidence_from_occurrences(&session_id, &occurrences, &policy).unwrap();
    assert_eq!(first, second);
    assert!(
        first
            .iter()
            .any(|record| record.evidence_kind() == DerivedEvidenceKindV1::Burst)
    );
    assert!(
        first
            .iter()
            .any(|record| record.evidence_kind() == DerivedEvidenceKindV1::Span)
    );
    let encoded = serde_json::to_value(&first).unwrap();
    assert_eq!(encoded, serde_json::to_value(&second).unwrap());

    let mut disordered = occurrences.clone();
    disordered.swap(0, 2);
    assert!(matches!(
        derive_session_evidence_from_occurrences(&session_id, &disordered, &policy),
        Err(SessionContractError::NoncontiguousDerivedEvidenceOrdinals)
    ));
}
