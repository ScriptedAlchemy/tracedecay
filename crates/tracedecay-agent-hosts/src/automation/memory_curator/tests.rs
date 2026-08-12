use super::*;
use tracedecay_domain::{
    DomainError, FactEventId, FactIdentityMaterialV1, FactIdentitySourceV1, ProvenanceId,
};
use tracedecay_store::{
    FactCommitReceipt, ProjectMemoryFactCurationOperationEffectV1,
    ProjectMemoryFactCurationReceiptV1, ProjectMemoryFactIdV1,
};

fn domain_id<T>(value: &str) -> T
where
    T: TryFrom<String, Error = DomainError>,
{
    T::try_from(value.to_owned()).unwrap()
}

fn settled_curation_receipt() -> ProjectMemoryFactCurationReceiptV1 {
    let owner = FactOwnerV1::Profile;
    let operation_id = domain_id::<ProvenanceId>("operation.curator.settled");
    let fact_id = FactId::derive(
        &FactIdentityMaterialV1::new(
            owner.clone(),
            FactIdentitySourceV1::Application {
                operation_id: operation_id.clone(),
            },
        )
        .unwrap(),
    )
    .unwrap();
    let fact_event_id = domain_id::<FactEventId>("event.curator.settled.fact");
    let provenance_event_id = domain_id::<FactEventId>("event.curator.settled.provenance");
    let commit = FactCommitReceipt::new(
        fact_id.clone(),
        owner.clone(),
        vec![fact_event_id, provenance_event_id.clone()],
        provenance_event_id,
        None,
    )
    .unwrap();
    ProjectMemoryFactCurationReceiptV1::new(
        owner.clone(),
        operation_id,
        "a".repeat(64),
        Some(tracedecay_domain::RunId::new("run.memory-curator-test").unwrap()),
        vec![
            ProjectMemoryFactCurationOperationEffectV1::normalize_tags(
                ProjectMemoryFactIdV1::new(owner.clone(), fact_id.clone()).unwrap(),
                commit,
            )
            .unwrap(),
        ],
        vec![ProjectMemoryFactIdV1::new(owner, fact_id).unwrap()],
    )
    .unwrap()
}

fn all_noop_curation_receipt() -> ProjectMemoryFactCurationReceiptV1 {
    let owner = FactOwnerV1::Profile;
    let derive_fact_id = |operation_id: &str| {
        FactId::derive(
            &FactIdentityMaterialV1::new(
                owner.clone(),
                FactIdentitySourceV1::Application {
                    operation_id: ProvenanceId::new(operation_id).unwrap(),
                },
            )
            .unwrap(),
        )
        .unwrap()
    };
    let duplicate = derive_fact_id("operation.curator.all-noop.duplicate");
    let absent = derive_fact_id("operation.curator.all-noop.absent");
    serde_json::from_value(json!({
        "owner": owner,
        "operation_id": ProvenanceId::new("operation.curator.all-noop").unwrap(),
        "input_digest": "b".repeat(64),
        "automation_run_id": tracedecay_domain::RunId::new("run.curator.all-noop").unwrap(),
        "operation_effects": [{
            "kind": "add",
            "fact_id": duplicate,
            "disposition": "near_duplicate",
            "closest_fact_id": duplicate,
            "similarity_millionths": 1_000_000,
            "commit": null,
        }, {
            "kind": "remove",
            "target_fact_id": absent,
            "disposition": "not_found",
            "remaining_fact_count": 0,
            "commit": null,
        }],
        "replay_fact_id": null,
        "replay_event_id": null,
        "changed_fact_ids": [],
        "accepted_operations": 2,
        "facts_added": 0,
        "facts_updated": 0,
        "facts_merged": 0,
        "facts_removed": 0,
        "normalized_tags": 0,
        "facts_linked": 0,
    }))
    .unwrap()
}

#[test]
fn invalid_authority_result_retains_exact_settled_curation_receipt() {
    let expected = settled_curation_receipt();
    let expected_json = serde_json::to_value(&expected).unwrap();
    let failure = settle_memory_curation_result(
        Err(MemoryMutationError::InvalidAuthorityResult {
            error: MemoryApplicationError::InvalidAuthorityResult {
                invariant: "curation receipt owner",
            },
            authority_result: expected,
        }),
        1,
    )
    .unwrap_err();

    let MemoryCurationApplyFailure::Settled {
        operation_count,
        receipt,
        ..
    } = failure
    else {
        panic!("invalid authority result must retain its settled receipt");
    };
    assert_eq!(operation_count, 1);
    assert_eq!(serde_json::to_value(&receipt).unwrap(), expected_json);
    assert_eq!(
        memory_curation_receipt_json("failed_after_partial_effects", 1, &receipt)["receipt"],
        expected_json
    );
}

#[test]
fn memory_curator_request_does_not_duplicate_review_messages() {
    let marker = "cluster-evidence-that-must-appear-once";
    let review = json!({
        "status": "needs_llm_review",
        "messages": [
            { "role": "system", "content": "return strict JSON" },
            { "role": "user", "content": marker },
        ],
    });

    let prompt = build_memory_curator_prompt();
    let request = AgentTaskRequest::new(
        "run-1".to_string(),
        AgentTaskKind::MemoryCurator,
        prompt.clone(),
        None,
        memory_curator_backend_context(&review, 0.8),
    );
    let backend_message = request.backend_message().unwrap();

    assert!(prompt.contains("canonical current facts"));
    for operation in [
        "\"op\":\"add\"",
        "\"op\":\"update\"",
        "\"op\":\"merge\"",
        "\"op\":\"remove\"",
        "\"op\":\"normalize_tags\"",
        "\"op\":\"link_facts\"",
    ] {
        assert!(prompt.contains(operation));
        assert!(build_memory_curator_repair_prompt().contains(operation));
    }
    assert!(prompt.contains("expected_last_event_id"));
    assert!(prompt.contains("at most 256 operations"));
    assert!(prompt.contains("\"general\"|\"user_pref\"|\"project\""));
    assert!(prompt.contains("1..=256 unique IDs"));
    assert!(prompt.contains("at most 4096 bytes"));
    assert!(prompt.contains("[context.min_confidence,1]"));
    assert_eq!(backend_message.matches(marker).count(), 1);
    assert_eq!(request.context["apply"], json!(true));
}

#[test]
fn all_noop_curation_is_settled_without_claiming_a_store_mutation() {
    let receipt = all_noop_curation_receipt();
    let (settled_count, mutation_count, _, committed) =
        settle_memory_curation_result(Ok(receipt), 2).unwrap();
    assert_eq!(settled_count, 2);
    assert_eq!(mutation_count, 0);
    assert_eq!(
        memory_curation_mutation_count(committed.as_ref().unwrap()),
        0
    );

    let authority = CurationApplyAuthorityV1 {
        actor_id: ActorId::new("automation:memory-curator").unwrap(),
        project_id: None,
        profile_id: tracedecay_domain::configuration::UserProfileId::new(
            "profile.curator.all-noop",
        )
        .unwrap(),
        configuration_revision_id: ConfigurationRevisionId::new("config.curator.all-noop.v1")
            .unwrap(),
    };
    let operations = vec![json!({"op": "add"}), json!({"op": "remove"})];
    let evidence = crate::automation::artifacts::sha256_bytes(b"all-noop curation evidence");
    let decision = memory_curation_decision(
        &AutomationConfig::default(),
        &authority,
        Some(&evidence),
        &operations,
    )
    .unwrap();
    let report = memory_curation_report(&operations, &decision, 2, 0, false);
    assert_eq!(report.pointer("/effect/accepted_count"), Some(&json!(2)));
    assert_eq!(report.pointer("/effect/settled_count"), Some(&json!(2)));
    assert_eq!(report.pointer("/effect/applied_count"), Some(&json!(0)));
    assert_eq!(report.pointer("/effect/mutates_store"), Some(&json!(false)));
    assert_eq!(report.pointer("/effect/fully_applied"), Some(&json!(true)));
    assert_eq!(
        memory_curation_settlement_status(&decision, 2, 2, 0),
        "settled_noop"
    );
    assert_eq!(
        memory_curation_settlement_status(&decision, 0, 0, 0),
        "no_candidate"
    );
}

#[test]
fn memory_curator_rejects_more_than_256_operations_before_validation() {
    let hostile_secret = "secret-that-must-not-enter-the-ledger";
    let output = json!({
        "ops": (0..257).map(|_| json!({
            "op": "add",
            "content": hostile_secret,
        })).collect::<Vec<_>>()
    });
    let (accepted, rejected) = validate_memory_curation_ops(&output, &BTreeMap::new(), 0.8);
    assert!(accepted.is_empty());
    assert_eq!(rejected.len(), 1);
    assert!(
        rejected[0]["rejected_reason"]
            .as_str()
            .unwrap()
            .contains("256-operation limit")
    );
    assert!(
        !serde_json::to_string(&rejected)
            .unwrap()
            .contains(hostile_secret)
    );
}

#[test]
fn durable_curation_validation_summary_is_payload_free() {
    let hostile_secret = "secret-model-content-never-persisted";
    let summary = memory_curation_validation_summary("failed", 4, 3, 1, 2, 0, 0);
    let encoded = serde_json::to_string(&summary).unwrap();
    assert!(!encoded.contains(hostile_secret));
    assert_eq!(summary["accepted_count"], json!(3));
    assert_eq!(summary["applied_count"], json!(0));
    assert_eq!(summary["mutates_store"], json!(false));
}

#[test]
fn memory_curator_review_contract_contains_no_local_similarity_authority() {
    let source = FactId::new(format!("fact.{}.{}", "0".repeat(64), "1".repeat(64))).unwrap();
    let review = memory_curator_review_value(
        vec![json!({
            "fact_id": source,
            "content": "Keep automatic curation on canonical fact content",
            "category": "decision",
            "tags": ["memory"],
            "trust": 0.9,
            "metadata": {},
        })],
        0,
        false,
    );

    assert_eq!(review["status"], json!("needs_llm_review"));
    assert_eq!(review["facts_reviewed"], json!(1));
    assert!(review.get("pairs").is_none());
    assert!(review.get("similarity_millionths").is_none());
}

#[test]
fn memory_curator_request_stays_below_codex_limit_for_large_review() {
    const CODEX_APP_SERVER_MAX_INPUT_CHARS: usize = 1_048_576;
    let review = json!({
        "status": "needs_llm_review",
        "messages": [
            { "role": "system", "content": "return strict JSON" },
            { "role": "user", "content": "x".repeat(600_000) },
        ],
    });
    let request = AgentTaskRequest::new(
        "run-1".to_string(),
        AgentTaskKind::MemoryCurator,
        build_memory_curator_prompt(),
        None,
        memory_curator_backend_context(&review, 0.8),
    );

    let backend_message = request.backend_message().unwrap();

    assert!(backend_message.len() < CODEX_APP_SERVER_MAX_INPUT_CHARS);
}

#[test]
fn memory_curator_accepts_all_six_canonical_operations_with_exact_cas() {
    let source = FactId::new(format!("fact.{}.{}", "0".repeat(64), "1".repeat(64))).unwrap();
    let target = FactId::new(format!("fact.{}.{}", "0".repeat(64), "2".repeat(64))).unwrap();
    let removable = FactId::new(format!("fact.{}.{}", "0".repeat(64), "3".repeat(64))).unwrap();
    let evidence = FactId::new(format!("fact.{}.{}", "0".repeat(64), "4".repeat(64))).unwrap();
    let source_event = FactEventId::new("event.curator.source".to_owned()).unwrap();
    let target_event = FactEventId::new("event.curator.target".to_owned()).unwrap();
    let removable_event = FactEventId::new("event.curator.removable".to_owned()).unwrap();
    let evidence_event = FactEventId::new("event.curator.evidence".to_owned()).unwrap();
    let allowed = BTreeMap::from([
        (source.clone(), source_event.clone()),
        (target.clone(), target_event.clone()),
        (removable.clone(), removable_event.clone()),
        (evidence.clone(), evidence_event.clone()),
    ]);
    let (accepted, rejected) = validate_memory_curation_ops(
        &json!({
            "ops": [{
                "op": "add",
                "content": "Canonical memory introduced by automatic curation",
                "category": "decision",
                "source_label": "automation:memory-curator",
                "tags": ["memory"],
                "entities": ["TraceDecay"],
                "trust": 0.9,
                "metadata": {},
                "evidence_facts": [{"fact_id": evidence, "expected_last_event_id": evidence_event}],
                "confidence": 0.92,
                "reason": "reviewed canonical evidence",
            }, {
                "op": "update",
                "target": {"fact_id": source, "expected_last_event_id": source_event},
                "tags": ["cache", "policy"],
                "evidence_facts": [{"fact_id": evidence, "expected_last_event_id": evidence_event}],
                "confidence": 0.91,
                "reason": "reviewed exact update",
            }, {
                "op": "merge",
                "winner": {
                    "fact_id": source,
                    "expected_last_event_id": source_event,
                },
                "losers": [{
                    "fact_id": target,
                    "expected_last_event_id": target_event,
                }],
                "merged_content": "Canonical merged fact",
                "evidence_facts": [{"fact_id": evidence, "expected_last_event_id": evidence_event}],
                "confidence": 0.93,
                "reason": "reviewed exact merge",
            }, {
                "op": "remove",
                "target": {"fact_id": removable, "expected_last_event_id": removable_event},
                "evidence_facts": [{"fact_id": evidence, "expected_last_event_id": evidence_event}],
                "confidence": 0.95,
                "reason": "reviewed exact removal",
            }, {
                "op": "normalize_tags",
                "target": {"fact_id": source, "expected_last_event_id": source_event},
                "tags": ["cache", "policy"],
                "evidence_facts": [{"fact_id": evidence, "expected_last_event_id": evidence_event}],
                "confidence": 0.9,
            }, {
                "op": "link_facts",
                "source": {"fact_id": source, "expected_last_event_id": source_event},
                "target": {"fact_id": target, "expected_last_event_id": target_event},
                "relation": "supports",
                "evidence_facts": [{"fact_id": evidence, "expected_last_event_id": evidence_event}],
                "confidence": 0.9,
                "source_label": "automation:memory-curator",
                "metadata": {},
            }]
        }),
        &allowed,
        0.8,
    );

    assert_eq!(accepted.len(), 6);
    assert!(rejected.is_empty());
}

#[test]
fn memory_curator_rejects_stale_or_missing_destructive_cas() {
    let fact = FactId::new(format!("fact.{}.{}", "0".repeat(64), "1".repeat(64))).unwrap();
    let evidence = FactId::new(format!("fact.{}.{}", "0".repeat(64), "2".repeat(64))).unwrap();
    let current_event = FactEventId::new("event.curator.current".to_owned()).unwrap();
    let allowed = BTreeMap::from([
        (fact.clone(), current_event),
        (
            evidence.clone(),
            FactEventId::new("event.curator.evidence".to_owned()).unwrap(),
        ),
    ]);
    let (accepted, rejected) = validate_memory_curation_ops(
        &json!({
            "ops": [{
                "op": "update",
                "target": {"fact_id": fact, "expected_last_event_id": "event.curator.stale"},
                "tags": ["stale"],
                "evidence_facts": [{"fact_id": evidence, "expected_last_event_id": "event.curator.evidence"}],
                "confidence": 0.9,
                "reason": "stale update",
            }, {
                "op": "remove",
                "target": {"fact_id": fact},
                "evidence_facts": [{"fact_id": evidence, "expected_last_event_id": "event.curator.evidence"}],
                "confidence": 0.9,
                "reason": "missing CAS",
            }]
        }),
        &allowed,
        0.8,
    );

    assert!(accepted.is_empty());
    assert_eq!(rejected.len(), 2);
}

#[test]
fn memory_curator_repair_rejects_invalid_source_labels_tags_and_review_snapshots() {
    let fact = FactId::new(format!("fact.{}.{}", "0".repeat(64), "1".repeat(64))).unwrap();
    let evidence = FactId::new(format!("fact.{}.{}", "0".repeat(64), "2".repeat(64))).unwrap();
    let event = FactEventId::new("event.curator.fact".to_owned()).unwrap();
    let evidence_event = FactEventId::new("event.curator.evidence".to_owned()).unwrap();
    let allowed = BTreeMap::from([
        (fact.clone(), event.clone()),
        (evidence.clone(), evidence_event.clone()),
    ]);
    let output = json!({"ops": [{
        "op": "add",
        "content": "safe",
        "category": "general",
        "source_label": "\u{0000}",
        "tags": [], "entities": [], "metadata": {},
        "evidence_facts": [{"fact_id": evidence, "expected_last_event_id": evidence_event}],
        "confidence": 0.9, "reason": "reviewed",
    }, {
        "op": "normalize_tags",
        "target": {"fact_id": fact, "expected_last_event_id": event},
        "tags": ["\u{0000}"],
        "evidence_facts": [{"fact_id": evidence, "expected_last_event_id": "event.stale"}],
        "confidence": 0.9,
    }]});

    let (accepted, rejected) = validate_memory_curation_ops(&output, &allowed, 0.8);
    assert!(accepted.is_empty());
    assert_eq!(rejected.len(), 2);
}

#[test]
fn memory_curator_quarantines_legacy_operations() {
    let allowed = BTreeMap::new();
    let (accepted, rejected) = validate_memory_curation_ops(
        &json!({
            "ops": [{
                "op": "delete",
                "fact_id": "fact.legacy",
                "confidence": 0.9,
            }, {
                "op": "merge_entities",
                "winner_entity_id": 1,
                "loser_entity_ids": [2],
                "evidence_fact_ids": [],
                "confidence": 0.9,
            }]
        }),
        &allowed,
        0.8,
    );

    assert!(accepted.is_empty());
    assert_eq!(rejected.len(), 2);
}
