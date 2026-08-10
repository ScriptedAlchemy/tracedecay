use axum::body::to_bytes;
use serde_json::{Map, Value, json};
use tower::ServiceExt;
use tracedecay_agent_hosts::automation::automatic_facts::{
    AutomaticFactState, record_session_automatic_facts,
};
use tracedecay_domain::{
    ComponentVersion, Confidence, FactCategoryV1, PayloadReferenceV1, ProvenanceId,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1,
};
use tracedecay_store::{ProjectMemoryAutomaticFactEvidenceV1, ProjectMemoryFactAddCommandV1};

use super::*;

fn admitted_fact(content: &str, metadata: Value) -> Value {
    json!({
        "add_fact_request": {
            "content": content,
            "category": "project",
            "source": "dashboard-route-test",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "trust": 0.9,
            "metadata": metadata,
        },
        "validation": {"status": "accepted"},
    })
}

async fn response_json(response: Response) -> Value {
    let body = to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("automatic fact receipt response body");
    serde_json::from_slice(&body).expect("automatic fact receipt response JSON")
}

fn accepted_fixture_receipt(material: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.dashboard-automatic-fact").expect("receipt id"),
            ComponentVersion::new("sanitizer.dashboard-fixture.v1").expect("sanitizer revision"),
        )
        .expect("receipt reference"),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(material).expect("payload reference")),
    )
    .expect("fixture receipt")
}

#[tokio::test]
async fn automatic_fact_receipt_routes_expose_only_terminal_authority_outcomes() {
    let fixture = DashboardStateFixture::open("project.dashboard-automatic-fact-receipts").await;
    let (applied_id, quarantined_id) = {
        let memory = memory_application_for_db(
            fixture.state.memory_owner.clone(),
            fixture.state.mem_db.as_ref(),
        )
        .expect("automatic fact receipt memory authority");
        let applied = record_session_automatic_facts(
            &memory,
            "run.dashboard-applied",
            Some("evidence.dashboard-applied"),
            &[admitted_fact(
                "Dashboard exposes the terminal applied automatic fact receipt",
                json!({"fixture": "applied"}),
            )],
        )
        .await
        .expect("applied automatic fact receipt");

        let sensitive_key = ["s", "k", "-test-123456"].concat();
        let quarantined_metadata = Value::Object(Map::from_iter([(
            sensitive_key,
            json!("credential-bearing object keys are never projected"),
        )]));
        let quarantined_content =
            "Dashboard exposes the terminal quarantined automatic fact receipt";
        let quarantined_material = json!({
            "content": quarantined_content,
            "category": "project",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "metadata": &quarantined_metadata,
        });
        let quarantined_apply_id =
            ProvenanceId::new("automatic-fact.dashboard-quarantined".to_owned())
                .expect("quarantined apply id");
        let quarantined_command = ProjectMemoryFactAddCommandV1::new(
            memory.owner().clone(),
            ProvenanceId::new("operation.dashboard-quarantined".to_owned())
                .expect("quarantined operation id"),
            quarantined_content.to_owned(),
            FactCategoryV1::Project,
            Some("dashboard-route-test".to_owned()),
            vec!["automation".to_owned()],
            vec!["TraceDecay".to_owned()],
            quarantined_metadata,
            accepted_fixture_receipt(&quarantined_material),
            Confidence::new(0.9).expect("quarantined default trust"),
            None,
        )
        .expect("quarantined automatic fact command")
        .with_automation_run_id("run.dashboard-quarantined".to_owned())
        .expect("quarantined automation run identity");
        let quarantined = memory
            .apply_project_memory_automatic_fact(
                quarantined_apply_id,
                quarantined_command,
                ProjectMemoryAutomaticFactEvidenceV1::new(
                    Some("evidence.dashboard-quarantined".to_owned()),
                    None,
                    Some(json!({"status": "accepted"})),
                )
                .expect("quarantined receipt evidence"),
            )
            .await
            .expect("quarantined automatic fact receipt");

        assert!(
            applied.retry_error.is_none(),
            "applied automatic fact retry error: {:?}",
            applied.retry_error
        );
        assert_eq!(applied.receipts.len(), 1);
        assert_eq!(applied.receipts[0].state, AutomaticFactState::Applied);
        assert_eq!(
            quarantined.receipt().state(),
            tracedecay_store::ProjectMemoryAutomaticFactStateV1::Quarantined
        );
        let projected = crate::util::query_i64_result(
            &fixture.state.mem_db.engine_conn(),
            "SELECT COUNT(*) FROM memory_v2_current_facts",
            (),
        )
        .await
        .expect("canonical automatic fact projection count");
        assert_eq!(
            projected, 1,
            "a quarantined receipt must not fabricate an applied projection"
        );

        (
            applied.receipts[0].apply_id.clone(),
            quarantined.receipt().apply_id().as_str().to_owned(),
        )
    };
    let app = router_with_active_application(fixture.state, None, Router::new());

    let list = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/automation/automatic-fact-receipts?limit=10")
                .body(Body::empty())
                .expect("automatic fact receipt list request"),
        )
        .await
        .expect("automatic fact receipt list response");
    assert_eq!(list.status(), StatusCode::OK);
    let list = response_json(list).await;
    assert_eq!(list["count"], 2);
    assert_eq!(list["limit"], 10);
    assert!(list["receipts"].as_array().is_some_and(|receipts| {
        receipts
            .iter()
            .all(|receipt| matches!(receipt["state"].as_str(), Some("applied" | "quarantined")))
    }));

    let quarantined = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/automation/automatic-fact-receipts?state=quarantined&limit=10")
                .body(Body::empty())
                .expect("quarantined receipt list request"),
        )
        .await
        .expect("quarantined receipt list response");
    assert_eq!(quarantined.status(), StatusCode::OK);
    let quarantined = response_json(quarantined).await;
    assert_eq!(quarantined["count"], 1);
    let receipt = &quarantined["receipts"][0];
    assert_eq!(receipt["apply_id"], quarantined_id);
    assert_eq!(receipt["state"], "quarantined");
    assert!(receipt.get("applied_canonical_fact_id").is_none());
    assert!(receipt.get("applied_fact_id").is_none());
    assert_eq!(
        receipt["quarantine_reason"],
        "content declined by privacy sanitizer"
    );

    for (id, expected_state) in [
        (applied_id.as_str(), "applied"),
        (quarantined_id.as_str(), "quarantined"),
    ] {
        let viewed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/automation/automatic-fact-receipts/{id}"))
                    .body(Body::empty())
                    .expect("automatic fact receipt view request"),
            )
            .await
            .expect("automatic fact receipt view response");
        assert_eq!(viewed.status(), StatusCode::OK);
        let viewed = response_json(viewed).await;
        assert_eq!(viewed["receipt"]["apply_id"], id);
        assert_eq!(viewed["receipt"]["state"], expected_state);
    }

    let invalid_state = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/automation/automatic-fact-receipts?state=pending")
                .body(Body::empty())
                .expect("invalid automatic fact receipt state request"),
        )
        .await
        .expect("invalid automatic fact receipt state response");
    assert_eq!(invalid_state.status(), StatusCode::BAD_REQUEST);

    let missing = app
        .oneshot(
            Request::builder()
                .uri("/api/automation/automatic-fact-receipts/automatic-fact.missing")
                .body(Body::empty())
                .expect("missing automatic fact receipt request"),
        )
        .await
        .expect("missing automatic fact receipt response");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}
