use crate::dashboard_api_support::*;

fn assert_canonical_fact_id(value: &Value, context: &str) {
    let raw = value
        .as_str()
        .unwrap_or_else(|| panic!("{context} must be a canonical fact-id string: {value}"));
    FactId::new(raw.to_owned())
        .unwrap_or_else(|error| panic!("{context} is not canonical: {error}: {raw}"));
}

fn retained_url(fixture: &DashboardFixture, operation: &str) -> String {
    format!("{}/api/application/retained/{operation}", fixture.base_url)
}

fn retained_effect_payload<'a>(response: &'a Value, operation: &str) -> &'a Value {
    assert_eq!(response["kind"], "success", "{operation}: {response}");
    assert_eq!(
        response["value"]["outcome"]["outcome"], "effect",
        "{operation} must return a canonical effect: {response}"
    );
    let effect = &response["value"]["outcome"]["value"];
    assert_eq!(effect["effect_class"], "administrative");
    assert_eq!(effect["reconciliation"], "reconciled");
    assert_eq!(effect["execution"]["termination"], "completed");
    assert_eq!(effect["receipt"]["outcome"], "completed");
    assert_eq!(effect["receipt"]["effect_class"], "administrative");
    assert_eq!(
        effect["receipt"]["operation"],
        format!(
            "use-case.application.retained.{}",
            operation.replace('_', "-")
        )
    );
    assert_eq!(
        effect["receipt"]["scope"]["scope_digest"],
        effect["authority"]["authorized_scope_digest"]
    );
    assert_eq!(
        effect["receipt"]["idempotency_key"],
        effect["idempotency_key"]
    );
    assert!(
        effect["authority"]["grant_id"].as_str().is_some_and(
            |grant| grant.starts_with("grant.tracedecay-daemon.project-open.retained.")
        )
    );
    assert!(effect["authority"]["grant_digest"].is_string());
    assert_eq!(
        effect["receipt"]["actor"],
        "actor.tracedecay-daemon.project-open"
    );
    assert!(effect["receipt"]["committed_state"].is_string());
    &effect["payload"]
}

#[test]
fn retained_admin_journey_commits_add_update_feedback_and_remove() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_retained_memory_fixture().await;
        let agent = http_agent();

        let (status, added) = post_json_body(
            &agent,
            &retained_url(&fixture, "fact_store_add"),
            &serde_json::json!({
                "content": "Dashboard retained administration uses canonical receipts",
                "memory_scope": "project",
                "category": "decision",
                "tags": ["dashboard", "retained"],
                "entities": ["TraceDecay"],
                "source_label": "dashboard-api-test",
                "metadata": {"journey": "retained-admin"}
            }),
        );
        assert_eq!(status, 200, "canonical fact add failed: {added}");
        let added_payload = retained_effect_payload(&added, "fact_store_add");
        assert_eq!(added_payload["outcome"], "committed");
        assert_eq!(added_payload["result"]["disposition"], "added");
        let added_fact = &added_payload["result"]["fact"];
        assert_eq!(added_fact["kind"], "available");
        assert_eq!(
            added_fact["fact"]["content"],
            "Dashboard retained administration uses canonical receipts"
        );
        assert_eq!(added_fact["fact"]["owner"]["kind"], "project");
        assert_eq!(
            added_fact["fact"]["owner"]["project_id"],
            fixture.host_runtime.project_id().as_str()
        );
        assert_canonical_fact_id(&added_fact["fact"]["fact_id"], "added fact_id");
        let fact_id = added_fact["fact"]["fact_id"]
            .as_str()
            .unwrap_or_else(|| panic!("add must return a canonical fact id: {added}"))
            .to_owned();
        let added_event_id = added_payload["result"]["commit"]["last_event_id"]
            .as_str()
            .unwrap_or_else(|| panic!("add must return a CAS event id: {added}"))
            .to_owned();
        assert_eq!(added_payload["result"]["commit"]["fact_id"], fact_id);

        let (status, updated) = post_json_body(
            &agent,
            &retained_url(&fixture, "fact_store_update"),
            &serde_json::json!({
                "fact_id": fact_id,
                "expected_last_event_id": added_event_id,
                "content": "Dashboard retained administration preserves canonical receipts",
                "tags": ["dashboard", "retained", "verified"],
                "metadata": {"journey": "retained-admin", "updated": true}
            }),
        );
        assert_eq!(status, 200, "canonical fact update failed: {updated}");
        let updated_payload = retained_effect_payload(&updated, "fact_store_update");
        assert_eq!(updated_payload["fact"]["kind"], "available");
        assert_eq!(updated_payload["fact"]["fact"]["fact_id"], fact_id);
        assert_eq!(
            updated_payload["fact"]["fact"]["content"],
            "Dashboard retained administration preserves canonical receipts"
        );
        let updated_event_id = updated_payload["commit"]["last_event_id"]
            .as_str()
            .unwrap_or_else(|| panic!("update must return a CAS event id: {updated}"))
            .to_owned();

        let (status, feedback) = post_json_body(
            &agent,
            &retained_url(&fixture, "fact_feedback"),
            &serde_json::json!({
                "fact_id": fact_id,
                "expected_last_event_id": updated_event_id,
                "action": "helpful",
                "source_label": "dashboard-api-test",
                "reason": "verified by the retained administration journey"
            }),
        );
        assert_eq!(status, 200, "canonical fact feedback failed: {feedback}");
        let feedback_payload = retained_effect_payload(&feedback, "fact_feedback");
        assert_eq!(feedback_payload["feedback"]["fact_id"], fact_id);
        assert_eq!(feedback_payload["feedback"]["action"], "helpful");
        assert!(
            feedback_payload["feedback"]["trust_delta_millionths"]
                .as_i64()
                .is_some_and(|delta| delta > 0)
        );
        assert_eq!(feedback_payload["feedback"]["helpful_count"], 1);
        let feedback_event_id = feedback_payload["commit"]["last_event_id"]
            .as_str()
            .unwrap_or_else(|| panic!("feedback must return a CAS event id: {feedback}"))
            .to_owned();

        let (status, removed) = post_json_body(
            &agent,
            &retained_url(&fixture, "fact_store_remove"),
            &serde_json::json!({
                "fact_id": fact_id,
                "expected_last_event_id": feedback_event_id
            }),
        );
        assert_eq!(status, 200, "canonical fact remove failed: {removed}");
        let removed_payload = retained_effect_payload(&removed, "fact_store_remove");
        assert_eq!(removed_payload["outcome"], "removed");
        assert_eq!(removed_payload["commit"]["fact_id"], fact_id);
        assert_eq!(removed_payload["fact"]["kind"], "unavailable");
        assert_eq!(removed_payload["fact"]["status"]["fact_id"], fact_id);
        assert_eq!(
            removed_payload["fact"]["status"]["payload_access"],
            "deleted"
        );

        let (status, final_read) = post_json_body(
            &agent,
            &retained_url(&fixture, "fact_store_get"),
            &serde_json::json!({"fact_id": fact_id}),
        );
        assert_eq!(status, 200, "canonical tombstone read failed: {final_read}");
        assert_eq!(final_read["kind"], "success");
        assert_eq!(
            final_read["value"]["outcome"]["outcome"], "evidence",
            "final read must preserve the canonical evidence envelope: {final_read}"
        );
        let final_payload = &final_read["value"]["outcome"]["value"]["payload"];
        assert_eq!(final_payload["fact"]["kind"], "unavailable");
        assert_eq!(final_payload["fact"]["status"]["fact_id"], fact_id);
        assert_eq!(final_payload["fact"]["status"]["payload_access"], "deleted");
        assert!(
            final_payload["trust_history"]
                .as_array()
                .is_some_and(|history| history.iter().any(|entry| {
                    entry["action"] == "helpful" && entry["source_label"] == "dashboard-api-test"
                })),
            "the tombstone read must preserve canonical feedback lineage: {final_read}"
        );
    });
}

#[test]
fn retained_mutations_deny_foreign_project_scope_without_a_receipt() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_retained_memory_fixture().await;
        let agent = http_agent();
        let (_target_root, target_cg) = crate::projects::setup_target_project(&fixture).await;
        let target_project_id = target_cg
            .store_layout()
            .identity
            .project_id
            .clone()
            .expect("registered target must have a project id");
        let denied_content =
            "crossboundarysentinel must not cross the admitted project boundary";
        let (status, denied) = post_json_body(
            &agent,
            &retained_url(&fixture, "fact_store_add"),
            &serde_json::json!({
                "content": denied_content,
                "memory_scope": "project",
                "project_selector": {
                    "project_id": target_project_id.clone()
                }
            }),
        );

        assert_eq!(
            status, 404,
            "foreign-project retained mutation must fail closed: {denied}"
        );
        assert_eq!(denied["kind"], "problem");
        assert_eq!(
            denied["value"]["problem"]["kind"],
            "not_found_or_not_authorized"
        );
        assert_eq!(denied["value"]["problem"]["owning_layer"], "application");
        assert!(
            denied["value"]["problem"]["committed_receipt"].is_null(),
            "denied mutation cannot report a committed effect: {denied}"
        );
        assert!(
            denied["value"].get("binding_id").is_none(),
            "concealed denial cannot expose the retained binding: {denied}"
        );

        for (store, url) in [
            (
                "active",
                format!(
                    "{}/api/plugins/holographic/?q=crossboundarysentinel&limit=10",
                    fixture.base_url
                ),
            ),
            (
                "registered target",
                format!(
                    "{}/api/projects/{target_project_id}/plugins/holographic/?q=crossboundarysentinel&limit=10",
                    fixture.base_url
                ),
            ),
        ] {
            let (read_status, payload) = get_json(&agent, &url);
            assert_eq!(
                read_status, 200,
                "{store} store must remain readable after the denied mutation: {payload}"
            );
            let holographic = &payload["payload"]["holographic"];
            assert_eq!(
                holographic["reads"]["facts"]["state"], "ready",
                "{store} fact authority must complete before absence is asserted: {payload}"
            );
            assert_eq!(
                holographic["facts_coverage"]["completeness"], "complete",
                "{store} fact coverage must be complete before absence is asserted: {payload}"
            );
            assert_eq!(
                holographic["facts"].as_array().map(Vec::len),
                Some(0),
                "denied mutation must not write the sentinel into the {store} store: {payload}"
            );
        }
    });
}

#[test]
fn automatic_fact_receipt_endpoints_expose_terminal_applied_and_quarantined_receipts() {
    use tracedecay::store::memory::DatabaseFactStore;
    use tracedecay_domain::{
        ActorId, ComponentVersion, FactOwnerV1, PayloadReferenceV1, ProvenanceId,
        SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
        SanitizerDispositionV1, SensitivityV1,
    };
    use tracedecay_store::{ProjectMemoryAutomaticFactEvidenceV1, ProjectMemoryFactAddMaterialV1};
    use tracedecay_usecases::memory::{
        MemoryApplication, ProjectMemoryFactAddRequest, automatic_fact_add_command,
    };

    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let cg = fixture
            .host_runtime
            .open_project_graph_for_test(
                &fixture.project_root,
                tracedecay::tracedecay::TraceDecayOpenOptions::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("open dashboard fixture project: {error}"));
        let project_id = cg
            .store_layout()
            .identity
            .project_id
            .as_deref()
            .and_then(|value| ProjectId::new(value.to_owned()).ok())
            .unwrap_or_else(|| panic!("dashboard fixture needs an authoritative project id"));
        let owner = FactOwnerV1::Project { project_id };
        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(cg.db()))
            .unwrap_or_else(|error| panic!("initialize automatic fact authority: {error}"));
        let apply_id = "automatic-fact-receipt-api-applied";
        let command = automatic_fact_add_command(
            owner.clone(),
            ProjectMemoryFactAddRequest {
                content: "Automatic receipt API preserves applied facts".to_owned(),
                category: FactCategoryV1::Decision,
                source_label: Some("dashboard-api-test".to_owned()),
                tags: Vec::new(),
                entities: Vec::new(),
                trust: Some(
                    Confidence::new(0.9)
                        .unwrap_or_else(|error| panic!("build automatic fact trust: {error}")),
                ),
                metadata: serde_json::json!({}),
            },
            "run.dashboard-receipt-api",
            apply_id,
            Some(
                ActorId::new("automation:dashboard-api-test".to_owned())
                    .unwrap_or_else(|error| panic!("build automatic fact actor: {error}")),
            ),
        )
        .unwrap_or_else(|error| panic!("build automatic fact command: {error}"));
        memory
            .apply_project_memory_automatic_fact(
                ProvenanceId::new(apply_id.to_owned())
                    .unwrap_or_else(|error| panic!("build automatic fact receipt id: {error}")),
                command,
                ProjectMemoryAutomaticFactEvidenceV1::default(),
                &test_fact_write_control(),
            )
            .await
            .unwrap_or_else(|error| panic!("record automatic applied receipt: {error}"));

        let quarantined_apply_id = "automatic-fact-receipt-api-quarantined";
        let quarantined_content = "api_key=0000000000000000";
        let quarantine_receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.dashboard-api-quarantined".to_owned())
                    .unwrap_or_else(|error| panic!("build sanitization receipt id: {error}")),
                ComponentVersion::new("sanitizer.dashboard-api-test.v1".to_owned())
                    .unwrap_or_else(|error| panic!("build sanitizer version: {error}")),
            )
            .unwrap_or_else(|error| panic!("build sanitization receipt reference: {error}")),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(
                PayloadReferenceV1::for_payload(&serde_json::json!({
                    "content": quarantined_content,
                    "category": FactCategoryV1::Decision,
                    "source_label": "dashboard-api-test",
                    "tags": [],
                    "entities": [],
                    "metadata": {},
                }))
                .unwrap_or_else(|error| panic!("bind sanitization payload: {error}")),
            ),
        )
        .unwrap_or_else(|error| panic!("build accepted sanitization receipt: {error}"));
        let quarantined_command = ProjectMemoryFactAddMaterialV1::new(
            owner,
            quarantined_content.to_owned(),
            FactCategoryV1::Decision,
            Some("dashboard-api-test".to_owned()),
            Vec::new(),
            Vec::new(),
            serde_json::json!({}),
            quarantine_receipt,
            Some("run.dashboard-receipt-api".to_owned()),
            Confidence::new(0.9).unwrap_or_else(|error| panic!("build fact confidence: {error}")),
            None,
        )
        .unwrap_or_else(|error| panic!("build quarantined automatic fact material: {error}"))
        .into_command(
            ProvenanceId::new("automatic-fact-receipt-api-quarantined-operation".to_owned())
                .unwrap_or_else(|error| panic!("build automatic operation id: {error}")),
        )
        .unwrap_or_else(|error| panic!("build quarantined automatic fact command: {error}"));
        memory
            .apply_project_memory_automatic_fact(
                ProvenanceId::new(quarantined_apply_id.to_owned()).unwrap_or_else(|error| {
                    panic!("build quarantined automatic fact receipt id: {error}")
                }),
                quarantined_command,
                ProjectMemoryAutomaticFactEvidenceV1::default(),
                &test_fact_write_control(),
            )
            .await
            .unwrap_or_else(|error| panic!("record automatic quarantined receipt: {error}"));

        let agent = http_agent();
        let endpoint = format!(
            "{}/api/automation/automatic-fact-receipts",
            fixture.base_url
        );
        let (status, listed) = get_json(&agent, &format!("{endpoint}?state=applied"));
        assert_eq!(status, 200, "automatic receipt list failed: {listed}");
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["limit"], 50);
        assert_eq!(listed["error"], "");
        let receipts = listed["receipts"]
            .as_array()
            .unwrap_or_else(|| panic!("automatic receipt list must contain receipts: {listed}"));
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0]["apply_id"], apply_id);
        assert_eq!(receipts[0]["run_id"], "run.dashboard-receipt-api");
        assert_eq!(receipts[0]["state"], "applied");
        assert!(
            receipts[0]["applied_fact_id"]
                .as_str()
                .is_some_and(|raw| FactId::new(raw.to_owned()).is_ok())
        );
        assert!(
            receipts[0]["recorded_at_micros"]
                .as_i64()
                .is_some_and(|micros| micros > 1_000_000_000_000),
            "automatic receipt list must preserve canonical microseconds: {}",
            receipts[0]
        );
        let (status, viewed) = get_json(&agent, &format!("{endpoint}/{apply_id}"));
        assert_eq!(status, 200, "automatic receipt view failed: {viewed}");
        assert_eq!(viewed["error"], "");
        assert_eq!(viewed["receipt"]["apply_id"], apply_id);
        assert_eq!(viewed["receipt"]["state"], "applied");
        assert_eq!(
            viewed["receipt"]["recorded_at_micros"], receipts[0]["recorded_at_micros"],
            "list and view must expose the same canonical receipt timestamp"
        );

        let (status, quarantined) = get_json(&agent, &format!("{endpoint}?state=quarantined"));
        assert_eq!(
            status, 200,
            "quarantined receipt list failed: {quarantined}"
        );
        assert_eq!(quarantined["count"], 1);
        assert_eq!(quarantined["receipts"][0]["apply_id"], quarantined_apply_id);
        assert_eq!(quarantined["receipts"][0]["state"], "quarantined");
        assert!(quarantined["receipts"][0]["quarantine_reason"].is_string());
        assert!(quarantined["receipts"][0].get("applied_fact_id").is_none());

        let (status, rejected_filter) = get_json(&agent, &format!("{endpoint}?state=staged"));
        assert_eq!(status, 400);
        assert!(
            rejected_filter["error"]
                .as_str()
                .is_some_and(|error| error.contains("expected applied or quarantined")),
            "non-terminal filter must be rejected: {rejected_filter}"
        );
        cg.close();
    });
}
