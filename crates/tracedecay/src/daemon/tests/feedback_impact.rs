//! `tracedecay_feedback_impact` as an MCP client sees it.
//!
//! The tool is a handle-addressed read. A diagnostics handle minted by a
//! published advisory cycle projects that cycle's identity and impact, and
//! nothing else. A handle that was never issued, or that was issued for a
//! different feedback read, is the same concealed refusal.
//!
//! A cycle whose every diagnostic provider is unavailable terminates
//! `daemon_unavailable` and mints no handle. The production way to move one
//! provider off that state is a real compiler warning admitted by
//! `tracedecay_diagnose`.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracedecay_mcp::JsonRpcResponse;

use crate::daemon::ProductionProjectCompositionHarnessV1;

const IMPACT_TOOL: &str = "tracedecay_feedback_impact";
const IMPACT_RESULT_SCHEMA: &str = "schema.application.feedback.impact.result";
const PROBE_SOURCE: &str =
    "pub fn feedback_impact_probe() -> i32 {\n    let unused_impact = 7;\n    0\n}\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_impact_projects_the_published_cycle_and_conceals_other_handles() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("source dir");
    std::fs::write(project.join("src/lib.rs"), PROBE_SOURCE).expect("source file");
    commit_project(&project);
    let head = git_head(&project);
    let document_uri = url::Url::from_file_path(project.join("src/lib.rs"))
        .expect("document file URI")
        .to_string();

    let harness = ProductionProjectCompositionHarnessV1::open(temp.path(), vec![project.clone()])
        .await
        .expect("production composition");

    assert_invalid_request(
        &harness
            .call_tool(
                &project,
                IMPACT_TOOL,
                json!({"request_handle": " not-a-handle", "format": "json"}),
            )
            .await
            .expect("whitespace handle call"),
        "application surface request handle is invalid",
    );
    assert_invalid_request(
        &harness
            .call_tool(&project, IMPACT_TOOL, json!({"format": "json"}))
            .await
            .expect("missing handle call"),
        "application surface request does not match its reviewed schema: missing field `request_handle`",
    );
    assert_invalid_request(
        &harness
            .call_tool(
                &project,
                IMPACT_TOOL,
                json!({
                    "request_handle": "rh_0123456789abcdef01234567",
                    "files": ["src/lib.rs"],
                    "format": "json"
                }),
            )
            .await
            .expect("unknown field call"),
        "application surface request does not match its reviewed schema: unknown field `files`, expected `request_handle`",
    );

    let absent = wait_for_feedback_owner(&harness, &project).await;
    assert_concealed_impact(&absent);

    let published = publish_advisory_cycle(&harness, &project, &document_uri).await;
    let cycle = &published["cycle"];
    let impact_handle = published["read_handles"]["impact_handle"]
        .as_str()
        .expect("published impact handle")
        .to_owned();
    let list_handle = published["read_handles"]["list_handle"]
        .as_str()
        .expect("published list handle")
        .to_owned();
    assert_ne!(
        impact_handle, list_handle,
        "a list handle must not be reusable as the impact handle"
    );

    let foreign = harness
        .call_tool(
            &project,
            IMPACT_TOOL,
            json!({"request_handle": list_handle, "format": "json"}),
        )
        .await
        .expect("list handle used as impact");
    assert_concealed_impact(&foreign);

    let impact = harness
        .call_tool(
            &project,
            IMPACT_TOOL,
            json!({"request_handle": impact_handle, "format": "json"}),
        )
        .await
        .expect("impact read");
    let envelope = successful_envelope(&impact);
    assert_eq!(
        envelope["contract"],
        json!({
            "schema_id": IMPACT_RESULT_SCHEMA,
            "schema_revision": 1
        })
    );
    let payload = &envelope["outcome"]["value"]["payload"];
    let expected = json!({
        "result_id": cycle["result_id"],
        "cycle_id": cycle["cycle_id"],
        "scope": cycle["scope"],
        "content_identity": cycle.get("content_identity").cloned().unwrap_or(Value::Null),
        "impact": cycle["impact"].clone(),
        "state": cycle["impact_state"].clone(),
    });
    assert_eq!(payload, &expected);
    assert_eq!(payload["scope"]["branch_ref"], json!("refs/heads/master"));
    assert_eq!(payload["scope"]["head_commit_id"], json!(head));
    let mut keys = payload
        .as_object()
        .expect("impact payload object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "content_identity",
            "cycle_id",
            "impact",
            "result_id",
            "scope",
            "state"
        ]
    );

    harness.shutdown().await;
}

fn assert_invalid_request(response: &JsonRpcResponse, detail: &str) {
    assert!(
        response.result.is_none(),
        "an invalid impact request must not return a tool result: {response:?}"
    );
    let error = response
        .error
        .as_ref()
        .expect("invalid impact request is a JSON-RPC error");
    assert_eq!(response.id, json!(1));
    assert_eq!(error.code, -32602);
    assert_eq!(
        error.message,
        format!(
            "tool project route failed: reason_code=application_surface_invalid_request retryable=false: {detail}"
        )
    );
    assert_eq!(
        error.data,
        Some(json!({
            "tool": IMPACT_TOOL,
            "reason_code": "application_surface_invalid_request",
            "retryable": false,
            "detail": detail,
            "kind": "invalid_request",
            "code": "application_surface_invalid_request"
        }))
    );
}

fn assert_concealed_impact(response: &JsonRpcResponse) {
    assert!(
        response.error.is_none(),
        "concealment is a tool result, not a JSON-RPC error: {response:?}"
    );
    let result = response.result.as_ref().expect("tool result");
    assert_eq!(result["isError"], json!(true));
    assert_eq!(result["content"][0]["type"], json!("text"));
    let envelope: Value = serde_json::from_str(
        result["content"][0]["text"]
            .as_str()
            .expect("concealed impact text"),
    )
    .expect("concealed impact envelope");
    let request_id = envelope["request_id"]
        .as_str()
        .expect("request id")
        .to_owned();
    assert!(
        request_id.starts_with("request."),
        "daemon-minted request id: {request_id}"
    );
    assert_eq!(
        envelope["contract"],
        json!({
            "schema_id": IMPACT_RESULT_SCHEMA,
            "schema_revision": 1
        })
    );
    let problem = json!({
        "revision": 1,
        "kind": "not_found_or_not_authorized",
        "code": "not_found_or_not_authorized",
        "message": "The requested resource was not found or is not authorized",
        "diagnostic": null,
        "committed_receipt": null,
        "owning_layer": "application",
        "terminality": "pre_admission",
        "retryable": false,
        "retry": "never",
        "retry_scope": null,
        "retry_after_millis": null,
        "cancellation_stage": null,
        "unavailable_classification": null,
        "execution_failure_classification": null,
        "request_id": request_id,
        "trace_id": request_id,
        "details": [],
        "legal_actions": [],
        "coverage": null
    });
    assert_eq!(result["problem"], problem);
    assert_eq!(envelope["problem"], problem);
}

async fn wait_for_feedback_owner(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> JsonRpcResponse {
    let deadline = Instant::now() + Duration::from_mins(1);
    loop {
        let response = harness
            .call_tool(
                project,
                IMPACT_TOOL,
                json!({
                    "request_handle": "rh_000000000000000000000000",
                    "format": "json"
                }),
            )
            .await
            .expect("absent impact handle");
        if response.error.is_none()
            && response
                .result
                .as_ref()
                .is_some_and(|result| result["problem"]["kind"] == "not_found_or_not_authorized")
        {
            return response;
        }
        let retryable_owner = response.result.as_ref().is_some_and(|result| {
            result["problem"]["code"] == "feedback.owner_unavailable"
                && result["problem"]["retryable"] == true
        });
        assert!(
            retryable_owner,
            "an unknown impact handle must stay concealed once the owner is mounted, or stay retryably unavailable before that: {response:?}"
        );
        assert!(
            Instant::now() < deadline,
            "feedback owner stayed unavailable: {response:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn publish_advisory_cycle(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    document_uri: &str,
) -> Value {
    let compiler_output = compiler_warning(project);
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match publish_compiler_warning(harness, project, &compiler_output).await {
            CompilerPublication::Published => {}
            CompilerPublication::StillSettling(detail) => {
                assert!(
                    Instant::now() < deadline,
                    "compiler diagnostics stayed unpublished: {detail}"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
        }

        let response = harness
            .call_tool(
                project,
                "tracedecay_feedback_advisory_cycle",
                json!({"document_uri": document_uri, "format": "json"}),
            )
            .await
            .expect("advisory cycle call");
        if response.error.is_none()
            && response
                .result
                .as_ref()
                .is_some_and(|result| result.get("isError") != Some(&json!(true)))
        {
            let payload = &successful_envelope(&response)["outcome"]["value"]["payload"];
            if payload["cycle"]["published"] == json!(true)
                && payload["read_handles"]["impact_handle"].is_string()
            {
                return payload.clone();
            }
            // The owner can answer before the compiler snapshot is the
            // generation the cycle reads. An unpublished `daemon_unavailable`
            // cycle is that window, not a successful impact proof.
            assert_eq!(
                payload["cycle"]["termination"],
                json!("daemon_unavailable"),
                "a settled cycle must publish the diagnostics handle: {payload}"
            );
        } else {
            let retryable = response.result.as_ref().is_some_and(|result| {
                result["problem"]["code"] == "feedback.advisory-cycle.unavailable"
                    && result["problem"]["retryable"] == true
            });
            assert!(
                retryable,
                "advisory cycle must publish or stay retryably unavailable: {response:?}"
            );
        }
        assert!(
            Instant::now() < deadline,
            "advisory cycle stayed unpublished: {response:?}"
        );
        // The next attempt republishes against the generation the cycle is
        // about to read, so a generation move cannot strand the warning.
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

enum CompilerPublication {
    Published,
    StillSettling(String),
}

fn compiler_warning(project: &Path) -> String {
    let output_dir = tempfile::TempDir::new().expect("compiler output dir");
    let output = Command::new("rustc")
        .current_dir(project)
        .args([
            "--crate-type=lib",
            "--edition=2024",
            "--emit=metadata",
            "--color=never",
            "src/lib.rs",
            "--out-dir",
        ])
        .arg(output_dir.path())
        .output()
        .expect("run rustc");
    let stderr = String::from_utf8(output.stderr).expect("rustc stderr utf-8");
    assert!(
        output.status.success(),
        "rustc must compile the probe with a warning, not an error: {stderr}"
    );
    assert!(
        stderr.contains("unused variable: `unused_impact`"),
        "the probe must emit the unused-variable warning the diagnostic store admits: {stderr}"
    );
    stderr
}

async fn publish_compiler_warning(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    compiler_output: &str,
) -> CompilerPublication {
    let response = harness
        .call_tool(
            project,
            "tracedecay_diagnose",
            json!({
                "cargo_output": compiler_output,
                "include_callers": false,
                "format": "json"
            }),
        )
        .await
        .expect("diagnose call");
    if response.error.is_some()
        || response
            .result
            .as_ref()
            .is_some_and(|result| result["isError"] == json!(true))
    {
        return CompilerPublication::StillSettling(format!("{response:?}"));
    }
    let body = tool_json(&response);
    let status = body["published"]["status"].as_str().unwrap_or("");
    match status {
        "published" => {
            assert_eq!(
                body["published"]["inserted"],
                json!(1),
                "one unused-variable warning must enter the diagnostic store: {body}"
            );
            let diagnostics = body["diagnostics"]
                .as_array()
                .expect("diagnose diagnostics");
            assert!(
                diagnostics.iter().any(|item| {
                    item["severity"] == json!("warning")
                        && item["message"] == json!("unused variable: `unused_impact`")
                        && item["file"]
                            .as_str()
                            .is_some_and(|file| file.ends_with("src/lib.rs"))
                }),
                "diagnose must report the compiler warning on src/lib.rs: {body}"
            );
            CompilerPublication::Published
        }
        "skipped" | "failed" => CompilerPublication::StillSettling(body.to_string()),
        _ => panic!("diagnose publication has no typed status: {body}"),
    }
}

fn tool_json(response: &JsonRpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "tool call must not be a JSON-RPC error: {response:?}"
    );
    let result = response.result.as_ref().expect("tool result");
    assert_ne!(result["isError"], json!(true), "{result}");
    serde_json::from_str(
        result["content"][0]["text"]
            .as_str()
            .expect("tool result text"),
    )
    .expect("tool result json")
}

fn successful_envelope(response: &JsonRpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "successful impact read must not be a JSON-RPC error: {response:?}"
    );
    let result = response.result.as_ref().expect("tool result");
    assert_ne!(result["isError"], json!(true), "{result}");
    assert_eq!(result["content"][0]["type"], json!("text"));
    serde_json::from_str(
        result["content"][0]["text"]
            .as_str()
            .expect("impact result text"),
    )
    .expect("impact result envelope")
}

fn commit_project(project: &Path) {
    let git = |arguments: &[&str]| {
        let status = Command::new("git")
            .current_dir(project)
            .args(arguments)
            .status()
            .expect("run git");
        assert!(status.success(), "git {arguments:?}");
    };
    git(&["init", "--quiet", "-b", "master"]);
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=TraceDecay Test",
        "-c",
        "user.email=tracedecay@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "test: seed feedback impact",
    ]);
}

fn git_head(project: &Path) -> String {
    let output = Command::new("git")
        .current_dir(project)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("read HEAD");
    assert!(output.status.success(), "git rev-parse HEAD");
    String::from_utf8(output.stdout)
        .expect("HEAD utf-8")
        .trim()
        .to_owned()
}
