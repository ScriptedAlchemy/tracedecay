use crate::dashboard_api_support::*;

#[test]
fn delivery_overview_serves_real_git_reads_and_typed_unmounted_authority() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        // The fixture root is already a registered git repository carrying
        // the authoritative identity marker; the server resolved its exact
        // scope from it at startup. This test only adds real history.
        let mut fixture = start_dashboard_fixture_without_memory().await;
        let project_root = fixture.project_root.clone();
        write_file(
            &project_root.join("src/lib.rs"),
            "pub fn delivery_fixture() -> &'static str { \"initial\" }\n",
        );
        commit_all(&project_root, "initial delivery fixture");
        // Host `git init` still defaults to `master` on Ubuntu CI images;
        // this test owns an attached `main` so the live-head assertion is
        // not host-default-branch noise.
        git(&project_root, &["branch", "-M", "main"]);
        write_file(
            &project_root.join("src/review.rs"),
            "pub fn review_context() -> bool { true }\n",
        );
        commit_all(&project_root, "serve delivery review context");
        write_file(
            &project_root.join("src/lib.rs"),
            "pub fn delivery_fixture() -> &'static str { \"working tree\" }\n",
        );

        let agent = http_agent();

        let (status, body) = get_json(
            &agent,
            &format!("{}/api/delivery/overview", fixture.base_url),
        );
        assert_eq!(status, 200, "delivery overview should resolve: {body}");
        assert_eq!(body["schema_revision"], 1);
        assert_eq!(body["domain_state"], "partial");

        assert_eq!(body["payload"]["changes"]["state"], "ready");
        assert_eq!(
            body["payload"]["changes"]["value"]["head"]["state"],
            "attached"
        );
        assert_eq!(
            body["payload"]["changes"]["value"]["head"]["branch"],
            "main"
        );
        assert_eq!(body["payload"]["changes"]["value"]["unstaged"], 1);
        assert!(
            body["payload"]["changes"]["value"]["changed_paths"]
                .as_array()
                .is_some_and(|paths| paths.iter().any(|path| path == "src/lib.rs"))
        );

        assert_eq!(body["payload"]["commits"]["state"], "ready");
        let commits = body["payload"]["commits"]["value"]["items"]
            .as_array()
            .unwrap_or_else(|| panic!("expected delivery commit items: {body}"));
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0]["subject"], "serve delivery review context");
        assert!(
            commits[0]["commit"]
                .as_str()
                .is_some_and(|value| value.len() == 40)
        );
        assert!(commits[0]["author_at_micros"].is_i64());

        assert_eq!(
            body["payload"]["generation_freshness"]["state"],
            "unavailable"
        );
        assert_eq!(
            body["payload"]["generation_freshness"]["required_authority"],
            "daemon code-index generation freshness authority"
        );

        for source in [
            "pull_requests",
            "review_comments",
            "ci_checks",
            "failure_localization",
            "releases",
        ] {
            assert_eq!(
                body["payload"][source]["state"], "unavailable",
                "{source} must be typed unavailable rather than empty: {body}"
            );
            assert!(
                body["payload"][source]["required_authority"]
                    .as_str()
                    .is_some_and(|authority| authority.contains("authority")),
                "{source} must name the missing composition seam: {body}"
            );
        }

        fixture.server.stop();
    });
}

#[test]
fn delivery_contract_exposes_typed_provider_rows_and_source_states() {
    let schema: serde_json::Value = serde_json::from_str(
        &dashboard::contract_schema::render_dashboard_contract_schema()
            .expect("render dashboard contract schema"),
    )
    .expect("parse dashboard contract schema");
    let definitions = schema["$defs"]
        .as_object()
        .expect("dashboard schema definitions");

    for definition in [
        "DeliveryPullRequestV1",
        "DeliveryPullRequestOperationV1",
        "DeliveryGitHubOperationSnapshotV1",
        "DeliveryReviewItemV1",
        "DeliveryReviewObservationV1",
        "DeliveryCiCheckV1",
        "DeliveryCiRunIdentityV1",
        "DeliveryReleaseV1",
        "DeliveryRateLimitCheckpointV1",
    ] {
        assert!(
            definitions.contains_key(definition),
            "missing typed Delivery schema {definition}"
        );
    }

    assert!(
        definitions["DeliveryPullRequestV1"]["properties"]
            .get("operations")
            .is_some(),
        "pull requests must retain provider-qualified operation evidence"
    );
    assert!(
        definitions["DeliveryReviewItemV1"]["properties"]
            .get("observations")
            .is_some(),
        "review rows must retain latest-attempt and last-complete observations"
    );
    for property in ["observation_id", "run"] {
        assert!(
            definitions["DeliveryCiCheckV1"]["properties"]
                .get(property)
                .is_some(),
            "CI rows must retain opaque {property} identity"
        );
    }
    for private_field in ["checkpoint", "body_anchor", "body_digest", "failure_anchor"] {
        assert!(
            definitions["DeliveryReviewObservationV1"]["properties"]
                .get(private_field)
                .is_none()
                && definitions["DeliveryCiCheckV1"]["properties"]
                    .get(private_field)
                    .is_none(),
            "private retained-source field {private_field} must not cross the dashboard wire"
        );
    }

    let projection = definitions
        .get("DeliveryProjectionV1_for_DeliveryPullRequestTimelineV1")
        .unwrap_or_else(|| {
            definitions
                .iter()
                .find_map(|(name, schema)| {
                    (name.starts_with("DeliveryProjectionV1")
                        && schema.to_string().contains("not_published"))
                    .then_some(schema)
                })
                .expect("typed Delivery projection schema")
        });
    let projection_schema = projection.to_string();
    for state in [
        "ready",
        "partial",
        "stale",
        "rate_limited",
        "failed",
        "denied",
        "not_published",
        "empty_measured",
        "unavailable",
    ] {
        assert!(
            projection_schema.contains(state),
            "Delivery projection schema must retain {state}: {projection_schema}"
        );
    }
}
