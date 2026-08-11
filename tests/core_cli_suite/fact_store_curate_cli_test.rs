use std::path::Path;
use std::process::Stdio;

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::common::{
    canonical_existing_path, spawn_tracedecay_daemon, tracedecay_command_with_home,
};

fn init_project(home: &Path, project: &Path) {
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn cli_curation_fixture() -> bool { true }\n",
    )
    .unwrap();
    let output = tracedecay_command_with_home(home)
        .arg("init")
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .expect("tracedecay init should run");
    assert!(
        output.status.success(),
        "tracedecay init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fact_store_tool_success(
    home: &Path,
    project: &Path,
    tool_name: &str,
    mut arguments: Value,
) -> Value {
    arguments
        .as_object_mut()
        .expect("fact-store arguments")
        .insert("format".to_owned(), json!("json"));
    let project_arg = project.to_string_lossy().to_string();
    let arguments = serde_json::to_string(&arguments).expect("fact-store arguments JSON");
    let output = tracedecay_command_with_home(home)
        .current_dir(project)
        .args([
            "tool",
            "--project",
            project_arg.as_str(),
            tool_name,
            "--args",
            arguments.as_str(),
            "--json",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("tracedecay tool should run");
    assert!(
        output.status.success(),
        "{tool_name} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mcp_result: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{tool_name} returned invalid MCP JSON: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let text = mcp_result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool_name} omitted canonical JSON content: {mcp_result}"));
    let envelope: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("{tool_name} returned invalid application JSON: {error}"));
    envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("{tool_name} omitted application payload: {envelope}"))
}

#[test]
fn fact_store_curate_cli_commits_and_replays_a_link_through_the_real_daemon() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    let _daemon = spawn_tracedecay_daemon(&home_path);
    init_project(&home_path, &project_path);

    let source = fact_store_tool_success(
        &home_path,
        &project_path,
        "fact_store_add",
        json!({
            "content": "CLI curation source fact",
            "category": "decision",
            "entities": ["CLI curation"]
        }),
    );
    let source_id = source["fact"]["fact"]["fact_id"]
        .as_str()
        .expect("source fact id")
        .to_owned();
    let target = fact_store_tool_success(
        &home_path,
        &project_path,
        "fact_store_add",
        json!({
            "content": "CLI curation target fact",
            "category": "decision",
            "entities": ["CLI curation"]
        }),
    );
    let target_id = target["fact"]["fact"]["fact_id"]
        .as_str()
        .expect("target fact id")
        .to_owned();
    let request = json!({
        "min_confidence": 0.9,
        "operations": [{
            "kind": "link_facts",
            "source_fact_id": source_id,
            "target_fact_id": target_id,
            "relation": "supports",
            "evidence_fact_ids": [source_id, target_id],
            "confidence": 0.97,
            "source_label": "cli-canonical-curation-test",
            "metadata": {"basis": "two retained project facts"}
        }]
    });

    let applied = fact_store_tool_success(
        &home_path,
        &project_path,
        "fact_store_curate",
        request.clone(),
    );
    assert_eq!(applied["owner"]["kind"], "project");
    assert_eq!(applied["normalized_tags"], 0);
    assert_eq!(applied["facts_linked"], 1);
    assert_eq!(applied["changed_fact_ids"], json!([source_id, target_id]));
    assert!(
        applied["commit_receipts"]
            .as_array()
            .is_some_and(|receipts| receipts.len() == 1
                && receipts[0]["disposition"] == "committed"
                && receipts[0]["committed_event_ids"]
                    .as_array()
                    .is_some_and(|event_ids| !event_ids.is_empty()))
    );

    let replayed = fact_store_tool_success(&home_path, &project_path, "fact_store_curate", request);
    assert_eq!(replayed["operation_id"], applied["operation_id"]);
    assert_eq!(replayed["input_digest"], applied["input_digest"]);
    assert!(replayed["commit_receipts"].as_array().is_some_and(
        |receipts| receipts.len() == 1 && receipts[0]["disposition"] == "idempotent_replay"
    ));
}
