//! Live TaskId-rooted evidence checks shared by the daemon and dashboard mounts.

use serde_json::Value;

use super::{
    DashboardProcess, ProductionDaemon, assert_canonical_envelope, assert_typed_problem,
    current_product_graph_request, post_dashboard_envelope, post_envelope, product_selection,
};

pub(super) fn assert_live_task_rooted_retrieval(
    agent: &ureq::Agent,
    fixture: &ProductionDaemon,
    dashboard: &DashboardProcess,
    observed_at: i64,
) {
    // `views` is the read the Work workspace opens with, so the preceding
    // mutation has to be visible through both mounts. This also captures the
    // immutable root identity that every evidence request below must preserve.
    let graph_request = current_product_graph_request(observed_at);
    let mut verified_version = None;
    for (label, (status, graph_read)) in [
        (
            "daemon work/views",
            post_envelope(
                agent,
                &fixture.external_url("/application/work/views"),
                fixture,
                &graph_request,
            ),
        ),
        (
            "dashboard api/work/views",
            post_dashboard_envelope(
                agent,
                &format!("{}/api/work/views", dashboard.base_url),
                &graph_request,
            ),
        ),
    ] {
        eprintln!("{label} -> {status} {graph_read}");
        assert_canonical_envelope(label, status, &graph_read);
        let evidence = &graph_read["value"]["outcome"]["value"];
        assert_eq!(
            graph_read["value"]["outcome"]["outcome"], "evidence",
            "{graph_read}"
        );
        assert_eq!(evidence["payload"]["mode"], "current", "{graph_read}");
        let items = evidence["payload"]["snapshot"]["graph"]["items"]
            .as_array()
            .unwrap_or_else(|| panic!("{label} must carry graph items: {graph_read}"));
        assert!(
            items
                .iter()
                .any(|item| item["input"]["task_id"] == "task.work-surface-conformance"),
            "the created task must be readable through {label}: {graph_read}"
        );
        let observed_version = evidence["payload"]["snapshot"]["verified_version"].clone();
        match &verified_version {
            Some(expected) => assert_eq!(
                &observed_version, expected,
                "both published mounts must expose the same verified Work graph"
            ),
            None => verified_version = Some(observed_version),
        }
    }
    let verified_version = verified_version.expect("verified Work graph identity");

    // `retrieve-evidence` is the TaskId-rooted read behind the dashboard's
    // selected-task evidence panel and the typed SDK operation. The new task
    // has no accepted attempt yet, so complete zero selected sources is a
    // measured result from the exact graph—not an unavailable authority or a
    // fabricated empty product graph. All temporal modes still bind the same
    // verified Work root; provider-session semantics begin only after an
    // accepted attempt publishes a qualified session relation.
    for (mode, temporal) in [
        ("current", serde_json::json!({ "kind": "current" })),
        (
            "as-of",
            serde_json::json!({ "kind": "as_of", "cutoff": observed_at }),
        ),
        ("evolution", serde_json::json!({ "kind": "evolution" })),
        ("forensic", serde_json::json!({ "kind": "forensic" })),
    ] {
        let evidence_request = request(&verified_version, temporal, observed_at);
        for (label, (status, body)) in responses(agent, fixture, dashboard, &evidence_request, mode)
        {
            eprintln!("{label} -> {status} {body}");
            assert_canonical_envelope(&label, status, &body);
            assert_eq!(body["value"]["outcome"]["outcome"], "evidence", "{body}");
            let payload = &body["value"]["outcome"]["value"]["payload"];
            assert_eq!(
                payload["task_id"], "task.work-surface-conformance",
                "{body}"
            );
            assert_eq!(payload["verified_version"], verified_version, "{body}");
            assert_eq!(payload["sources"], serde_json::json!([]), "{body}");
            assert_eq!(payload["omissions"], serde_json::json!([]), "{body}");
            assert_eq!(payload["continuations"], serde_json::json!([]), "{body}");
            assert_eq!(payload["coverage"]["state"], "complete", "{body}");
            assert_eq!(payload["coverage"]["selected"], 0, "{body}");
            assert_eq!(payload["coverage"]["hydrated"], 0, "{body}");
            assert_eq!(payload["coverage"]["omitted"], 0, "{body}");
            assert_eq!(payload["freshness"], "current", "{body}");
        }
    }

    let evidence_request = request(
        &verified_version,
        serde_json::json!({ "kind": "current" }),
        observed_at,
    );
    let mut stale_request = evidence_request.clone();
    stale_request["verified_version"]["event_sequence"] = serde_json::json!(2);
    for (label, (status, body)) in
        responses(agent, fixture, dashboard, &stale_request, "stale graph")
    {
        eprintln!("{label} -> {status} {body}");
        assert_typed_problem(&label, status, &body, (409, "stale", true));
    }

    let mut missing_task_request = evidence_request;
    missing_task_request["task_id"] = serde_json::json!("task.work-surface-missing");
    for (label, (status, body)) in responses(
        agent,
        fixture,
        dashboard,
        &missing_task_request,
        "missing task",
    ) {
        eprintln!("{label} -> {status} {body}");
        assert_typed_problem(
            &label,
            status,
            &body,
            (404, "not_found_or_not_authorized", false),
        );
    }
}

fn request(verified_version: &Value, temporal: Value, observed_at: i64) -> Value {
    serde_json::json!({
        "selection": product_selection(),
        "task_id": "task.work-surface-conformance",
        "verified_version": verified_version,
        "temporal": temporal,
        "page_size": 25,
        "expansion": null,
        "continuation": null,
        "observed_at": observed_at,
    })
}

fn responses(
    agent: &ureq::Agent,
    fixture: &ProductionDaemon,
    dashboard: &DashboardProcess,
    request: &Value,
    state: &str,
) -> [(String, (u16, Value)); 2] {
    let qualifier = format!(" ({state})");
    [
        (
            format!("daemon work/retrieve-evidence{qualifier}"),
            post_envelope(
                agent,
                &fixture.external_url("/application/work/retrieve-evidence"),
                fixture,
                request,
            ),
        ),
        (
            format!("dashboard api/work/retrieve-evidence{qualifier}"),
            post_dashboard_envelope(
                agent,
                &format!("{}/api/work/retrieve-evidence", dashboard.base_url),
                request,
            ),
        ),
    ]
}
