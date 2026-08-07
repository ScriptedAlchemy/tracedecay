use crate::dashboard_api_support::*;

#[test]
fn delivery_overview_serves_real_git_reads_and_typed_missing_authorities() {
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
            "TraceDecay::last_synced_commit from the mounted project graph"
        );

        for source in [
            "pull_requests",
            "review_comments",
            "ci_checks",
            "failure_localization",
        ] {
            assert_eq!(
                body["payload"][source]["state"], "unavailable",
                "{source} must be typed unavailable rather than empty: {body}"
            );
            assert!(
                body["payload"][source]["required_authority"]
                    .as_str()
                    .is_some_and(|authority| authority.contains("DashboardState")),
                "{source} must name the missing composition seam: {body}"
            );
        }
        assert_eq!(body["payload"]["releases"]["state"], "unsupported");
        assert!(
            body["payload"]["releases"]["required_authority"]
                .as_str()
                .is_some_and(|authority| authority.contains("release"))
        );

        fixture.server.stop();
    });
}
