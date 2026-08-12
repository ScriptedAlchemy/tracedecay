mod common;

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::application_surface::{ApplicationSurfaceOperation, ApplicationSurfaceRequest};
use tracedecay::daemon::{DaemonHandshake, call_default_tool};
use tracedecay::daemon_client::{DaemonInvocationClient, RequestedOutputFormat};
use tracedecay::mcp::tools::dispatch::resolve_mcp_application_surface;
use tracedecay_application::retained_surfaces::RetainedSurfaceResultV1;
use tracedecay_application::{ApplicationEnvelope, RequestId};
use tracedecay_usecases::primitives::{PrimitiveRequest, StorageStatusPrimitiveRequest};

fn initialize_project(home: &Path, project: &Path) {
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/context_eval_project"),
        project,
    );
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/managed_run_overlay"),
        project,
    );
    let output = common::tracedecay_command_with_home(home)
        .arg("init")
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .expect("run tracedecay init");
    assert_command_success("tracedecay init", &output);
}

fn copy_dir(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("fixture destination");
    for entry in std::fs::read_dir(source).expect("fixture directory") {
        let entry = entry.expect("fixture entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy checked-in fixture");
        }
    }
}

fn assert_command_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn storage_status_request() -> ApplicationSurfaceRequest {
    ApplicationSurfaceRequest::Primitive(PrimitiveRequest::StorageStatus(
        StorageStatusPrimitiveRequest {
            include_details: false,
        },
    ))
}

fn admitted_project_id(home: &Path, project: &Path) -> String {
    let project_arg = project.to_string_lossy().into_owned();
    let output = common::tracedecay_command_with_home(home)
        .current_dir(project)
        .args([
            "tool",
            "--project",
            project_arg.as_str(),
            "storage_status",
            "--args",
            r#"{"include_details":false}"#,
            "--json",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run storage_status");
    assert_command_success("read admitted project identity", &output);
    let envelope: Value =
        serde_json::from_slice(&output.stdout).expect("storage status application envelope");
    envelope["scope"]["project_id"]
        .as_str()
        .unwrap_or_else(|| panic!("admission omitted its public project id: {envelope}"))
        .to_owned()
}

fn project_handshake(project: &Path) -> DaemonHandshake {
    DaemonHandshake::for_current_client(Some(project.to_path_buf()), None, false, false)
        .expect("project daemon handshake")
}

async fn assert_client_project_identity(
    client: &DaemonInvocationClient,
    expected_project_id: &str,
    request_id: &str,
) {
    let status = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::StorageStatus,
        RequestId::new(request_id).expect("storage identity request id"),
        storage_status_request(),
        RequestedOutputFormat::Json,
        Some(client),
    )
    .await
    .expect("fresh daemon client storage-status dispatch");
    let scope = &status
        .result
        .as_ref()
        .unwrap_or_else(|problem| panic!("fresh daemon client was rejected: {problem:?}"))
        .scope;
    assert_eq!(scope.project_id.as_str(), expected_project_id);
}

async fn fact_store_payload(
    handshake: &DaemonHandshake,
    operation: &str,
    label: &str,
    arguments: Value,
) -> Value {
    let tool = format!("tracedecay_fact_store_{operation}");
    let result = call_default_tool(handshake, &tool, arguments)
        .await
        .unwrap_or_else(|error| panic!("{label} failed: {error}"));
    tracedecay::daemon::tool_json_payload(&result, &tool)
        .unwrap_or_else(|error| panic!("{label} returned no public JSON payload: {error}"))
}

fn application_payload<'a>(envelope: &'a Value, expected_outcome: &str) -> &'a Value {
    serde_json::from_value::<ApplicationEnvelope<RetainedSurfaceResultV1>>(envelope.clone())
        .unwrap_or_else(|error| panic!("invalid retained application envelope: {error}"));
    assert!(
        envelope["request_id"]
            .as_str()
            .is_some_and(|request_id| !request_id.is_empty()),
        "application envelope omitted its request id: {envelope}"
    );
    assert_eq!(envelope["outcome"]["outcome"], expected_outcome);
    &envelope["outcome"]["value"]["payload"]
}

fn assert_logical_retry_receipts(first: &Value, replay: &Value) {
    let first_request = first["request_id"]
        .as_str()
        .expect("first delivery request id");
    let replay_request = replay["request_id"]
        .as_str()
        .expect("replay delivery request id");
    assert_ne!(
        first_request, replay_request,
        "logical retry must use a fresh transport request"
    );

    let mut first_receipt = first["outcome"]["value"]["receipt"].clone();
    let mut replay_receipt = replay["outcome"]["value"]["receipt"].clone();
    assert_eq!(first_receipt["request_id"], first["request_id"]);
    assert_eq!(replay_receipt["request_id"], replay["request_id"]);
    first_receipt
        .as_object_mut()
        .expect("first effect receipt")
        .remove("request_id");
    replay_receipt
        .as_object_mut()
        .expect("replay effect receipt")
        .remove("request_id");
    assert_eq!(
        first_receipt, replay_receipt,
        "logical retry changed the durable effect receipt"
    );
}

fn available_fact(projection: &Value) -> &Value {
    assert_eq!(
        projection["kind"], "available",
        "unavailable fact: {projection}"
    );
    projection
        .get("fact")
        .unwrap_or_else(|| panic!("available projection omitted its fact: {projection}"))
}

fn assert_sha256(value: &Value, label: &str) {
    assert!(
        value.as_str().is_some_and(|digest| {
            digest.len() == 71
                && digest.starts_with("sha256:")
                && digest[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
        }),
        "{label} is not a canonical sha256 digest: {value}"
    );
}

#[derive(Clone, Debug, PartialEq)]
struct AddedFactEvidence {
    fact_id: String,
    content: String,
    entity: String,
    tags: Value,
    metadata: Value,
    owner: Value,
    source_label: String,
    operation_id: String,
    committed_event_ids: Value,
    last_event_id: String,
    active_assertion_id: String,
    effect_id: String,
    idempotency_key: String,
    expected_state: Value,
    committed_state: Value,
}

fn added_fact_evidence(
    envelope: &Value,
    content: &str,
    entity: &str,
    expected_owner: Value,
    expected_commit_disposition: &str,
) -> AddedFactEvidence {
    let effect = &envelope["outcome"]["value"];
    let payload = application_payload(envelope, "effect");
    assert_eq!(effect["effect_class"], "administrative");
    assert_eq!(effect["receipt"]["outcome"], "completed");
    assert_eq!(payload["outcome"], "committed");
    let result = &payload["result"];
    assert_eq!(result["disposition"], "added");
    let fact = available_fact(&result["fact"]);
    let commit = &result["commit"];
    assert_eq!(fact["content"], content);
    assert!(fact["fact_id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(
        fact["entities"]
            .as_array()
            .is_some_and(|entities| { entities.iter().any(|candidate| candidate == entity) })
    );
    assert_eq!(commit["disposition"], expected_commit_disposition);
    assert_eq!(commit["fact_id"], fact["fact_id"]);
    assert_eq!(commit["owner"], expected_owner);
    assert_eq!(fact["owner"], expected_owner);
    let event_ids = commit["committed_event_ids"]
        .as_array()
        .filter(|event_ids| !event_ids.is_empty())
        .unwrap_or_else(|| panic!("commit omitted event identities: {payload}"));
    assert_eq!(event_ids.last(), Some(&commit["last_event_id"]));
    assert_eq!(commit["last_event_id"], fact["last_event_id"]);
    assert_eq!(commit["active_assertion_id"], fact["active_assertion_id"]);
    assert_eq!(fact["source"]["kind"], "application");
    assert_eq!(fact["source_label"], "physical-daemon-restart-acceptance");
    assert!(
        fact["source"]["operation_id"]
            .as_str()
            .is_some_and(|operation_id| !operation_id.is_empty())
    );
    assert_eq!(
        effect["idempotency_key"],
        effect["receipt"]["idempotency_key"]
    );
    assert_eq!(
        effect["expected_state"],
        effect["receipt"]["expected_state"]
    );
    assert_sha256(&effect["receipt"]["input_digest"], "effect input digest");
    assert_sha256(
        &effect["receipt"]["committed_state"],
        "effect committed-state digest",
    );
    AddedFactEvidence {
        fact_id: fact["fact_id"].as_str().unwrap().to_owned(),
        content: content.to_owned(),
        entity: entity.to_owned(),
        tags: fact["tags"].clone(),
        metadata: fact["metadata"].clone(),
        owner: expected_owner,
        source_label: fact["source_label"].as_str().unwrap().to_owned(),
        operation_id: fact["source"]["operation_id"].as_str().unwrap().to_owned(),
        committed_event_ids: commit["committed_event_ids"].clone(),
        last_event_id: commit["last_event_id"].as_str().unwrap().to_owned(),
        active_assertion_id: commit["active_assertion_id"].as_str().unwrap().to_owned(),
        effect_id: effect["effect_id"].as_str().unwrap().to_owned(),
        idempotency_key: effect["idempotency_key"].as_str().unwrap().to_owned(),
        expected_state: effect["expected_state"].clone(),
        committed_state: effect["receipt"]["committed_state"].clone(),
    }
}

fn relation_snapshot(payload: &Value, added: &AddedFactEvidence) -> Option<Value> {
    if payload["graph_coverage"]["kind"] != "complete"
        || payload["graph_coverage"]["relation_count"]
            .as_u64()
            .unwrap_or_default()
            == 0
    {
        return None;
    }
    let hit = payload["hits"]
        .as_array()?
        .iter()
        .find(|hit| hit["fact"]["fact_id"] == added.fact_id)?;
    let fact = &hit["fact"];
    assert_eq!(fact["content"], added.content);
    assert!(
        fact["entities"]
            .as_array()
            .is_some_and(|entities| { entities.iter().any(|entity| entity == &added.entity) })
    );
    assert_eq!(fact["tags"], added.tags);
    assert_eq!(fact["metadata"], added.metadata);
    assert_eq!(fact["owner"], added.owner);
    assert_eq!(fact["source"]["kind"], "application");
    assert_eq!(fact["source"]["operation_id"], added.operation_id);
    assert_eq!(fact["source_label"], added.source_label);
    Some(json!({
        "fact_id": fact["fact_id"],
        "content": fact["content"],
        "entities": fact["entities"],
        "tags": fact["tags"],
        "metadata": fact["metadata"],
        "owner": fact["owner"],
        "source": fact["source"],
        "source_label": fact["source_label"],
    }))
}

async fn wait_for_related_fact(
    handshake: &DaemonHandshake,
    added: &AddedFactEvidence,
    memory_scope: &str,
    label: &str,
) -> Value {
    let mut last_payload = Value::Null;
    for _ in 0..100 {
        let arguments = json!({
            "entity": added.entity,
            "memory_scope": memory_scope,
            "limit": 20,
            "min_trust": 0.0,
            "format": "json",
        });
        let envelope = fact_store_payload(handshake, "related", label, arguments).await;
        let payload = application_payload(&envelope, "evidence");
        if let Some(snapshot) = relation_snapshot(payload, added) {
            return snapshot;
        }
        last_payload = payload.clone();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{label} never reached complete graph coverage: {last_payload}");
}

async fn assert_related_absent(
    handshake: &DaemonHandshake,
    entity: &str,
    memory_scope: &str,
    label: &str,
) {
    let mut last_payload = Value::Null;
    for _ in 0..100 {
        let envelope = fact_store_payload(
            handshake,
            "related",
            label,
            json!({
                "entity": entity,
                "memory_scope": memory_scope,
                "limit": 20,
                "min_trust": 0.0,
                "format": "json",
            }),
        )
        .await;
        let payload = application_payload(&envelope, "evidence");
        if payload["graph_coverage"]["kind"] == "complete" {
            assert!(
                payload["hits"].as_array().is_some_and(Vec::is_empty),
                "{label} leaked relation evidence: {payload}"
            );
            return;
        }
        last_payload = payload.clone();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{label} never reached complete graph coverage: {last_payload}");
}

async fn get_fact(
    handshake: &DaemonHandshake,
    fact_id: &str,
    memory_scope: &str,
    label: &str,
) -> Value {
    let envelope = fact_store_payload(
        handshake,
        "get",
        label,
        json!({
            "fact_id": fact_id,
            "memory_scope": memory_scope,
            "format": "json",
        }),
    )
    .await;
    available_fact(&application_payload(&envelope, "evidence")["fact"]).clone()
}

async fn get_selected_project_fact(
    handshake: &DaemonHandshake,
    project_id: &str,
    fact_id: &str,
    label: &str,
) -> Value {
    let envelope = fact_store_payload(
        handshake,
        "get",
        label,
        json!({
            "fact_id": fact_id,
            "memory_scope": "project",
            "project_selector": {"project_id": project_id},
            "format": "json",
        }),
    )
    .await;
    assert_eq!(envelope["scope"]["project_id"], project_id);
    let fact = available_fact(&application_payload(&envelope, "evidence")["fact"]);
    assert_eq!(fact["fact_id"], fact_id);
    assert_eq!(fact["owner"]["kind"], "project");
    assert_eq!(fact["owner"]["project_id"], project_id);
    fact.clone()
}

async fn assert_selected_project_write_denied(
    handshake: &DaemonHandshake,
    project_id: &str,
    content: &str,
    entity: &str,
) {
    let tool = "tracedecay_fact_store_add";
    let result = call_default_tool(
        handshake,
        tool,
        json!({
            "content": content,
            "entities": [entity],
            "memory_scope": "project",
            "project_selector": {"project_id": project_id},
            "format": "json",
        }),
    )
    .await
    .expect("selected-project write denial transport");
    assert_eq!(result["isError"], true, "selected-project write succeeded");
    let problem = tracedecay::daemon::tool_json_payload(&result, tool)
        .expect("selected-project write denial problem");
    serde_json::from_value::<tracedecay_application::ApplicationProblemEnvelope>(problem.clone())
        .unwrap_or_else(|error| panic!("invalid selected-project denial envelope: {error}"));
    assert_eq!(problem["problem"]["kind"], "not_found_or_not_authorized");
}

fn telemetry(fact: &Value) -> Value {
    json!({
        "retrieval_count": fact["telemetry"]["retrieval_count"],
        "access_count": fact["telemetry"]["access_count"],
        "last_retrieved_at": fact["telemetry"]["last_retrieved_at"],
        "last_recalled_at": fact["telemetry"]["last_recalled_at"],
    })
}

async fn context_payload(handshake: &DaemonHandshake, task: &str, label: &str) -> Value {
    let result = call_default_tool(
        handshake,
        "tracedecay_context",
        json!({
            "task": task,
            "format": "json",
            "memory_limit": 20,
            "memory_min_trust": 0.0,
        }),
    )
    .await
    .unwrap_or_else(|error| panic!("{label} failed: {error}"));
    tracedecay::daemon::tool_json_payload(&result, "tracedecay_context")
        .unwrap_or_else(|error| panic!("{label} returned no JSON payload: {error}"))
}

async fn assert_context_matches_fact(
    handshake: &DaemonHandshake,
    fact_id: &str,
    task: &str,
    label: &str,
) {
    let payload = context_payload(handshake, task, label).await;
    assert!(
        payload["memory_matches"].as_array().is_some_and(|matches| {
            matches
                .iter()
                .any(|hit| hit["fact"]["fact_id"].as_str() == Some(fact_id))
        }),
        "{label} omitted fact {fact_id}: {payload}"
    );
}

async fn explicit_search(handshake: &DaemonHandshake, query: &str, fact_id: &str, label: &str) {
    let envelope = fact_store_payload(
        handshake,
        "search",
        label,
        json!({
            "query": query,
            "memory_scope": "project",
            "limit": 20,
            "min_trust": 0.0,
            "format": "json",
        }),
    )
    .await;
    let payload = application_payload(&envelope, "evidence");
    assert!(payload["hits"].as_array().is_some_and(|hits| {
        hits.iter()
            .any(|hit| hit["fact"]["fact_id"].as_str() == Some(fact_id))
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_relation_graph_survives_physical_daemon_restart_and_isolates_profile_and_projects()
{
    const A_CONTENT: &str = "Restart project A retains its mounted relation";
    const A_ENTITY: &str = "RestartRelationProjectAlpha";
    const B_CONTENT: &str = "Restart project B retains only its control relation";
    const B_ENTITY: &str = "RestartRelationProjectBeta";
    const PROFILE_CONTENT: &str = "Restart profile retains its mounted relation";
    const PROFILE_ENTITY: &str = "RestartRelationProfileOnly";
    const DENIED_SELECTED_CONTENT: &str = "Selected project writes stay denied";
    const DENIED_SELECTED_ENTITY: &str = "DeniedSelectedProjectWrite";

    let (environment, project_a) = common::IsolatedEnv::acquire().await;
    let project_b = environment.scratch().join("project-b");
    std::fs::create_dir_all(&project_b).expect("project B root");
    let mut daemon = common::spawn_tracedecay_daemon(environment.home());
    initialize_project(environment.home(), &project_a);
    initialize_project(environment.home(), &project_b);
    let project_a_id = admitted_project_id(environment.home(), &project_a);
    let project_b_id = admitted_project_id(environment.home(), &project_b);
    assert_ne!(project_a_id, project_b_id);

    let first_a = project_handshake(&project_a);
    let first_b = project_handshake(&project_b);
    let first_a_client =
        DaemonInvocationClient::for_current(first_a.clone()).expect("project A client");
    let first_b_client =
        DaemonInvocationClient::for_current(first_b.clone()).expect("project B client");
    assert_client_project_identity(
        &first_a_client,
        &project_a_id,
        "request.memory-restart.first-project-a",
    )
    .await;
    assert_client_project_identity(
        &first_b_client,
        &project_b_id,
        "request.memory-restart.first-project-b",
    )
    .await;

    let add_a = || {
        json!({
            "content": A_CONTENT,
            "category": "project",
            "tags": ["restart-project-alpha"],
            "entities": [A_ENTITY],
            "trust": 0.9,
            "source_label": "physical-daemon-restart-acceptance",
            "metadata": {
                "tags": "caller-metadata-tag-remains-separate",
                "marker": "project-alpha-metadata"
            },
            "format": "json",
        })
    };
    let added_a_envelope =
        fact_store_payload(&first_a, "add", "add project A relation fact", add_a()).await;
    let added_a = added_fact_evidence(
        &added_a_envelope,
        A_CONTENT,
        A_ENTITY,
        json!({"kind": "project", "project_id": project_a_id}),
        "committed",
    );
    assert_eq!(added_a.tags, json!(["restart-project-alpha"]));
    assert_eq!(
        added_a.metadata,
        json!({
            "tags": "caller-metadata-tag-remains-separate",
            "marker": "project-alpha-metadata"
        })
    );
    let replayed_a_envelope =
        fact_store_payload(&first_a, "add", "replay project A relation fact", add_a()).await;
    let replayed_a = added_fact_evidence(
        &replayed_a_envelope,
        A_CONTENT,
        A_ENTITY,
        json!({"kind": "project", "project_id": project_a_id}),
        "idempotent_replay",
    );
    assert_eq!(
        replayed_a, added_a,
        "logical retry changed durable evidence"
    );
    assert_logical_retry_receipts(&added_a_envelope, &replayed_a_envelope);

    let added_b = added_fact_evidence(
        &fact_store_payload(
            &first_b,
            "add",
            "add project B relation fact",
            json!({
                "content": B_CONTENT,
                "category": "project",
                "entities": [B_ENTITY],
                "trust": 0.9,
                "source_label": "physical-daemon-restart-acceptance",
                "format": "json",
            }),
        )
        .await,
        B_CONTENT,
        B_ENTITY,
        json!({"kind": "project", "project_id": project_b_id}),
        "committed",
    );
    let added_profile = added_fact_evidence(
        &fact_store_payload(
            &first_a,
            "add",
            "add profile relation fact",
            json!({
                "content": PROFILE_CONTENT,
                "memory_scope": "user",
                "category": "user_pref",
                "entities": [PROFILE_ENTITY],
                "trust": 0.9,
                "source_label": "physical-daemon-restart-acceptance",
                "format": "json",
            }),
        )
        .await,
        PROFILE_CONTENT,
        PROFILE_ENTITY,
        json!({"kind": "profile"}),
        "committed",
    );

    let initial_a = wait_for_related_fact(&first_a, &added_a, "project", "initial A").await;
    let initial_b = wait_for_related_fact(&first_b, &added_b, "project", "initial B").await;
    let selected_b = get_selected_project_fact(
        &first_a,
        &project_b_id,
        &added_b.fact_id,
        "selected B through A",
    )
    .await;
    assert_eq!(selected_b["owner"], added_b.owner);
    assert_eq!(selected_b["content"], B_CONTENT);
    assert_selected_project_write_denied(
        &first_a,
        &project_b_id,
        DENIED_SELECTED_CONTENT,
        DENIED_SELECTED_ENTITY,
    )
    .await;
    assert_related_absent(
        &first_b,
        DENIED_SELECTED_ENTITY,
        "project",
        "selected-project write denial remains absent from B",
    )
    .await;
    let initial_profile =
        wait_for_related_fact(&first_a, &added_profile, "user", "initial profile").await;

    let before_context = get_fact(&first_a, &added_a.fact_id, "project", "baseline get").await;
    assert_context_matches_fact(
        &first_a,
        &added_a.fact_id,
        A_ENTITY,
        "background context memory read",
    )
    .await;
    let after_context = get_fact(&first_a, &added_a.fact_id, "project", "post-context get").await;
    assert_eq!(telemetry(&after_context), telemetry(&before_context));

    explicit_search(
        &first_a,
        "RestartRelationProjectAlpha mounted relation",
        &added_a.fact_id,
        "explicit tracked search",
    )
    .await;
    let after_search = get_fact(&first_a, &added_a.fact_id, "project", "post-search get").await;
    assert_eq!(
        after_search["telemetry"]["retrieval_count"].as_u64(),
        before_context["telemetry"]["retrieval_count"]
            .as_u64()
            .map(|count| count + 1)
    );
    assert_eq!(
        after_search["telemetry"]["access_count"].as_u64(),
        before_context["telemetry"]["access_count"]
            .as_u64()
            .map(|count| count + 1)
    );
    assert!(!after_search["telemetry"]["last_retrieved_at"].is_null());
    assert!(!after_search["telemetry"]["last_recalled_at"].is_null());

    let first_daemon_pid = daemon.id();
    let killed = daemon
        .kill_and_wait()
        .expect("force-stop and reap the first physical daemon");
    assert!(
        !killed.success(),
        "forced daemon termination exited cleanly"
    );
    drop((first_a_client, first_b_client));
    daemon = common::spawn_tracedecay_daemon(environment.home());
    assert_ne!(daemon.id(), first_daemon_pid);

    let restarted_a = project_handshake(&project_a);
    let restarted_b = project_handshake(&project_b);
    let restarted_a_client =
        DaemonInvocationClient::for_current(restarted_a.clone()).expect("restarted A client");
    let restarted_b_client =
        DaemonInvocationClient::for_current(restarted_b.clone()).expect("restarted B client");
    assert_client_project_identity(
        &restarted_a_client,
        &project_a_id,
        "request.memory-restart.second-project-a",
    )
    .await;
    assert_client_project_identity(
        &restarted_b_client,
        &project_b_id,
        "request.memory-restart.second-project-b",
    )
    .await;

    assert_eq!(
        wait_for_related_fact(&restarted_a, &added_a, "project", "restarted A").await,
        initial_a
    );
    assert_eq!(
        wait_for_related_fact(&restarted_b, &added_b, "project", "restarted B").await,
        initial_b
    );
    assert_eq!(
        get_selected_project_fact(
            &restarted_a,
            &project_b_id,
            &added_b.fact_id,
            "restarted selected B through A",
        )
        .await,
        selected_b
    );
    assert_eq!(
        wait_for_related_fact(&restarted_a, &added_profile, "user", "restarted profile").await,
        initial_profile
    );
    assert_related_absent(&restarted_a, B_ENTITY, "project", "A excludes B").await;
    assert_related_absent(
        &restarted_a,
        PROFILE_ENTITY,
        "project",
        "A excludes profile",
    )
    .await;
    assert_related_absent(&restarted_b, A_ENTITY, "project", "B excludes A").await;
    assert_related_absent(
        &restarted_b,
        PROFILE_ENTITY,
        "project",
        "B excludes profile",
    )
    .await;
    assert_related_absent(&restarted_a, A_ENTITY, "user", "profile excludes A").await;
    assert_related_absent(&restarted_b, B_ENTITY, "user", "profile excludes B").await;

    let restarted_fact = get_fact(
        &restarted_a,
        &added_a.fact_id,
        "project",
        "restart telemetry get",
    )
    .await;
    assert_eq!(telemetry(&restarted_fact), telemetry(&after_search));
    assert_context_matches_fact(
        &restarted_a,
        &added_a.fact_id,
        A_ENTITY,
        "restarted background context memory read",
    )
    .await;
    let after_restart_context = get_fact(
        &restarted_a,
        &added_a.fact_id,
        "project",
        "post-restart-context get",
    )
    .await;
    assert_eq!(
        telemetry(&after_restart_context),
        telemetry(&restarted_fact)
    );

    drop((restarted_a_client, restarted_b_client, daemon));
}
