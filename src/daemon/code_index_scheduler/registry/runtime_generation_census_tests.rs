//! Mounted sealed-generation runtime-census journey coverage.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::CodeIndexSchedulerRegistryV1;
use crate::config::PinnedUserDataDir;
use crate::daemon::project_open_owners::{
    project_code_index_generation_census_reader, resolved_scope_for_project,
};
use crate::mcp::tools::handlers::{
    ToolCallRegistryOptions, handle_tool_call_with_registry_and_implicit_project,
};
use crate::runtime_telemetry::{GenerationCensusSnapshot, GenerationCensusUnavailableReason};
use crate::tracedecay::TraceDecay;

#[tokio::test]
async fn runtime_mcp_reports_exact_counts_from_the_mounted_sealed_generation() {
    let _profile = PinnedUserDataDir::new();
    let dir = TempDir::new().expect("fixture root");
    let project = dir.path().join("runtime-generation-census-observed");
    let source = "fn alpha() {}\nfn beta() { alpha(); }\n";
    fs::create_dir_all(project.join("src")).expect("create fixture source root");
    fs::write(project.join("src/lib.rs"), source).expect("write fixture source");
    run_git_in(&project, &["init", "-q", "-b", "main"]);
    run_git_in(&project, &["add", "."]);
    run_git_in(&project, &["commit", "-qm", "sealed generation fixture"]);
    let project_id = tracedecay_domain::ProjectId::new("project.mcp-runtime-generation-census")
        .expect("fixture project identity");
    let (cg, _runtime) =
        TraceDecay::init_test_fixture_with_registered_runtime(&project, project_id.as_str())
            .await
            .expect("open v32 mounted runtime fixture");
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    let mut publications = schedulers.subscribe_generation_publications();
    let sealed_store = dir.path().join("sealed-code-index");
    schedulers
        .mount_worktree(project_id.clone(), &project, sealed_store, None)
        .await
        .expect("mount sealed code-index generation");
    let scope =
        resolved_scope_for_project(&project, &project_id).expect("resolve exact fixture scope");
    let generation_census_reader =
        project_code_index_generation_census_reader(schedulers.clone(), project.clone(), scope);

    if schedulers.latest_generation_id(&project).await.is_none() {
        let canonical_project = project.canonicalize().expect("canonical fixture root");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let publication = publications.recv().await.expect("generation publication");
                if publication.project_root == canonical_project {
                    break;
                }
            }
        })
        .await
        .expect("sealed generation must publish for the mounted root");
    }

    let result = handle_tool_call_with_registry_and_implicit_project(
        &cg,
        "tracedecay_runtime",
        json!({ "format": "json" }),
        None,
        None,
        ToolCallRegistryOptions {
            generation_census_reader: Some(generation_census_reader),
            ..Default::default()
        },
    )
    .await
    .expect("mounted runtime census dispatch");
    let payload: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("runtime JSON text"),
    )
    .expect("parse runtime JSON");

    assert_eq!(
        payload["database"]["generation_census"],
        json!({
            "state": "observed",
            "source_total_bytes": 37,
            "symbol_count": 2,
            "edge_count": 1,
        }),
        "the runtime census must describe the sealed generation, not a removed relational graph"
    );

    let wrong_project_id =
        tracedecay_domain::ProjectId::new("project.mcp-runtime-generation-census-wrong-scope")
            .expect("wrong-scope project identity");
    let wrong_scope = resolved_scope_for_project(&project, &wrong_project_id)
        .expect("resolve wrong fixture scope");
    let wrong_scope_reader = project_code_index_generation_census_reader(
        schedulers.clone(),
        project.clone(),
        wrong_scope,
    );
    assert!(matches!(
        wrong_scope_reader().await,
        GenerationCensusSnapshot::Unavailable {
            reason: GenerationCensusUnavailableReason::ExactScopeGenerationNotReady,
        }
    ));
    let wrong_root_reader = project_code_index_generation_census_reader(
        schedulers.clone(),
        dir.path().join("unmounted-root"),
        resolved_scope_for_project(&project, &project_id).expect("resolve mounted fixture scope"),
    );
    assert!(matches!(
        wrong_root_reader().await,
        GenerationCensusSnapshot::Unavailable {
            reason: GenerationCensusUnavailableReason::ExactScopeGenerationNotReady,
        }
    ));

    schedulers.shutdown().await;
    cg.checkpoint().await.expect("checkpoint fixture database");
    cg.close();
}

fn run_git_in(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
