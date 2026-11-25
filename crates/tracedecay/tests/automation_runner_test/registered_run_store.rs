//! The registered run store is the single authority for automation run state:
//! runner writes land in it and survive reopening, every reader serves what it
//! holds, and a retired JSONL ledger is refused rather than served.

use crate::support::*;

use tracedecay_contracts::graph_tool::GraphToolResultV1;
use tracedecay_mcp::handlers::automation_runs::{compute_run_list, compute_run_view};

/// Runs the memory curator on a project with nothing to review, which settles
/// one terminal `skipped` record through the production runner and returns it.
async fn publish_runner_terminal(cg: &TraceDecay) -> AutomationRunLedgerRecord {
    let config = AutomationSettingsV1 {
        enabled: true,
        backend: AutomationBackendV1::CodexAppServer,
        host_mode: AutomationHostModeV1::Standalone,
        tasks: AutomationTaskSetV1 {
            memory_curator: AutomationTaskSettingsV1 {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskSettingsV1::default()
            },
            ..AutomationTaskSetV1::default()
        },
        ..AutomationSettingsV1::default()
    };
    let backend = JsonBackend::new(json!({"ops": []}));
    let run = settle_memory_curator(
        &automation_project_context(cg),
        &config,
        &test_configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            fact_review_limit: 4,
            min_confidence: 0.5,
            run_id: None,
        },
        &test_automation_run_control(Arc::new(AtomicBool::new(false))),
    )
    .await
    .unwrap();
    assert_eq!(
        backend.calls(),
        0,
        "nothing to review must not call the backend"
    );
    run.ledger_record
}

#[tokio::test]
async fn runner_publishes_and_reloads_registered_records() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let record = publish_runner_terminal(&cg).await;

    let stored = run_store(&cg)
        .find_run_record(&record.run_id)
        .await
        .unwrap();
    assert_eq!(
        stored.as_ref(),
        Some(&record),
        "the runner's terminal record is not in the registered store"
    );
    assert_eq!(record.status, AutomationRunStatus::Skipped);
    assert_eq!(record.error.as_deref(), Some("nothing_to_review"));
    assert!(
        !cg.store_layout()
            .dashboard_root
            .join("automation_runs.jsonl")
            .exists(),
        "the runner must not leave a file ledger beside the registered store"
    );

    // A freshly admitted handle over the same project reads the same history.
    let readmitted = run_store(&cg).load_run_records(10).await.unwrap();
    assert_eq!(readmitted, vec![record.clone()]);

    // Closing and reopening the whole project must still serve the record.
    let project_root = cg.project_root().to_path_buf();
    cg.close();
    let reopened =
        TraceDecay::open_with_options(&project_root, fixture_open_options(&project_root))
            .await
            .unwrap();
    let reloaded = run_store(&reopened);
    assert_eq!(
        reloaded.find_run_record(&record.run_id).await.unwrap(),
        Some(record.clone()),
        "the registered record did not survive reopening the project"
    );
    assert_eq!(reloaded.load_run_records(10).await.unwrap(), vec![record]);
    reopened.close();
}

#[tokio::test]
async fn mcp_cli_and_dashboard_read_the_same_registered_history() {
    // This target reaches the MCP run handlers and the store the CLI reads
    // through. The dashboard route handlers are private to
    // `tracedecay-dashboard-api` and need its HTTP state, so they are covered
    // by `dashboard_api_test`; they read via the same
    // `RegisteredAutomationRunStore` admitted here.
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let record = publish_runner_terminal(&cg).await;
    let store = run_store(&cg);

    let listed = compute_run_list(&store, &json!({"limit": 10}))
        .await
        .unwrap();
    let GraphToolResultV1::AutomationRunList(list) = listed.result else {
        panic!("run list must produce an automation run list");
    };
    let listed_ids = list
        .runs
        .iter()
        .map(|run| run.run_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        listed_ids,
        vec![record.run_id.as_str()],
        "a reader served history the registered store does not hold"
    );
    assert!(!list.has_more);
    assert_eq!(list.runs[0].status, record.status);
    assert_eq!(list.runs[0].error, record.error);

    let viewed = compute_run_view(&store, &json!({"run_id": record.run_id}))
        .await
        .unwrap();
    let GraphToolResultV1::AutomationRunView(view) = viewed.result else {
        panic!("run view must produce an automation run view");
    };
    assert_eq!(
        view.run, record,
        "a reader served history the registered store does not hold"
    );

    // The CLI-facing store read and a second admission agree with MCP.
    let page = store.load_run_records_page(10).await.unwrap();
    assert_eq!(page.records, vec![view.run.clone()]);
    let second_admission = run_store(&cg);
    assert_eq!(
        second_admission
            .find_run_record(&record.run_id)
            .await
            .unwrap(),
        Some(view.run)
    );

    // A run the store does not hold is not invented by any reader.
    let missing = compute_run_view(&store, &json!({"run_id": "never-published"})).await;
    assert!(missing.is_err(), "an absent run must be reported as absent");
}

fn legacy_artifact(dashboard_root: &Path, name: &str, directory: bool) -> std::path::PathBuf {
    let path = dashboard_root.join(name);
    if directory {
        fs::create_dir_all(&path).unwrap();
        let marker = path.join("pending.jsonl");
        fs::write(&marker, b"{\"run_id\":\"legacy-run\"}\n").unwrap();
        marker
    } else {
        fs::write(
            &path,
            b"{\"run_id\":\"legacy-run\",\"schema_version\":1}\nnot json\n",
        )
        .unwrap();
        path
    }
}

#[tokio::test]
async fn wrong_project_and_legacy_jsonl_refuse_without_touching_bytes() {
    let temp = tempdir().unwrap();
    let owner_root = temp.path().join("owner");
    let other_root = temp.path().join("other");
    fs::create_dir_all(&owner_root).unwrap();
    fs::create_dir_all(&other_root).unwrap();
    let owner = RunStoreFixture::open_project(&owner_root, "project.automation.owner").await;
    let other = RunStoreFixture::open_project(&other_root, "project.automation.other").await;
    let published = scheduler_record(
        "owner-run",
        AutomationRunStatus::Succeeded,
        current_timestamp() - 60,
    );
    owner.runs.append_run_record(&published).await.unwrap();

    // Every retired ledger artifact refuses admission and keeps its bytes.
    fs::create_dir_all(owner.dashboard_root()).unwrap();
    for (name, directory) in [
        ("automation_runs.jsonl", false),
        ("automation_runs.jsonl.append-intent", false),
        ("automation_run_spool", true),
        ("automation_run_spool.lock", false),
        ("automation_run_append_intent_quarantine", true),
    ] {
        let legacy_bytes_path = legacy_artifact(owner.dashboard_root(), name, directory);
        let before = fs::read(&legacy_bytes_path).unwrap();

        let refusal = owner.readmit();

        assert!(
            matches!(
                &refusal,
                Err(error) if error.project_route_context().map(|(code, _, _)| code)
                    == Some("automation_run_legacy_jsonl_refused")
            ),
            "legacy JSONL was served instead of refused: {name} -> {refusal:?}"
        );
        assert_eq!(
            fs::read(&legacy_bytes_path).unwrap(),
            before,
            "refusing {name} must leave its bytes unchanged"
        );

        let retired = owner.dashboard_root().join(name);
        if directory {
            fs::remove_dir_all(retired).unwrap();
        } else {
            fs::remove_file(retired).unwrap();
        }
    }
    owner
        .readmit()
        .expect("a dashboard root without retired ledger files is admitted");

    // A different project's shard never sees the owner's run.
    assert!(
        other
            .runs
            .find_run_record(&published.run_id)
            .await
            .unwrap()
            .is_none(),
        "a different project's store served another project's run"
    );
    assert_eq!(other.runs.load_run_records(10).await.unwrap(), Vec::new());
    assert_eq!(
        owner.runs.find_run_record(&published.run_id).await.unwrap(),
        Some(published),
        "the owning project must keep serving its own run"
    );
}
