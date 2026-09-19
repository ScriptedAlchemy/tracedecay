//! Serving status of `tracedecay_health_read` through the production MCP
//! `tools/call` the daemon mounts.
//!
//! The tool takes no selector. A writable serving file answers `ok`. Removing
//! that file answers `degraded`. Mode `0444` does not become `read_only`:
//! admission still opens the store as writable, owner publication cannot
//! persist, and the same call returns the typed owner-failed problem. Windows
//! sharing locks do not expose the sealed file the same way.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_mcp::JsonRpcResponse;

use super::support::{TestTempDir, test_temp_dir};

struct HealthProject {
    harness: ProductionProjectCompositionHarnessV1,
    project_root: PathBuf,
    // Moved into the sealed reopen. Dropping it earlier removes the serving
    // database out from under the daemon.
    isolation: TestTempDir,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_read_reports_ok_until_the_serving_database_is_gone() {
    let fixture = open_health_project().await;

    let markdown = call_health_text(&fixture, json!({})).await;
    assert_eq!(
        &markdown[..MARKDOWN_OK.len()],
        MARKDOWN_OK,
        "default MCP rendering must lead with the writable serving status:\n{markdown}"
    );
    assert!(
        markdown.contains("binding=binding.mcp.health_read.v1"),
        "default MCP rendering must name the MCP binding:\n{markdown}"
    );

    assert_eq!(
        call_health_payload(&fixture, json!({"format": "json"})).await,
        json!({"status": "ok"})
    );

    let unknown = call_health(&fixture, json!({"format": "json", "surprise": true})).await;
    let unknown = unknown
        .error
        .as_ref()
        .expect("an unknown argument must be a JSON-RPC error, not a status");
    assert_eq!(unknown.code, -32602);
    assert_eq!(
        unknown.message,
        "tool project route failed: reason_code=application_surface_invalid_request retryable=false: application surface request does not match its reviewed schema: unknown field `surprise`, there are no fields"
    );
    assert_eq!(
        unknown.data,
        Some(json!({
            "tool": "tracedecay_health_read",
            "reason_code": "application_surface_invalid_request",
            "retryable": false,
            "detail": "application surface request does not match its reviewed schema: unknown field `surprise`, there are no fields",
            "kind": "invalid_request",
            "code": "application_surface_invalid_request"
        }))
    );

    let bad_format = call_health(&fixture, json!({"format": "yaml"})).await;
    let bad_format = bad_format
        .error
        .as_ref()
        .expect("an unknown format must be a JSON-RPC error, not markdown");
    assert_eq!(bad_format.code, -32603);
    assert_eq!(
        bad_format.message,
        "tool execution failed: config error: application surface request does not match its reviewed schema: `format` must be markdown or json"
    );

    let database = serving_database_path(&fixture).await;
    fs::remove_file(&database).expect("unlink the serving database");
    assert!(
        !database.is_file(),
        "the degraded call must observe a missing serving database"
    );
    assert_eq!(
        call_health_payload(&fixture, json!({"format": "json"})).await,
        json!({"status": "degraded"})
    );
    let degraded = call_health_text(&fixture, json!({})).await;
    assert_eq!(
        &degraded[..MARKDOWN_DEGRADED.len()],
        MARKDOWN_DEGRADED,
        "default MCP rendering must lead with the missing-database status:\n{degraded}"
    );

    fixture.harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_read_refuses_when_the_serving_database_cannot_be_written() {
    let writable = open_health_project().await;
    assert_eq!(
        call_health_payload(&writable, json!({"format": "json"})).await,
        json!({"status": "ok"})
    );
    let sealed = seal_serving_database(writable).await;
    let problem = call_health_problem(&sealed, json!({"format": "json"})).await;
    assert_eq!(
        problem.schema_id,
        "schema.application.primitive.health-read.result"
    );
    assert_eq!(problem.kind, "execution_failed");
    assert_eq!(problem.code, "application.runtime.owner_failed");
    assert_eq!(problem.retry, "never");
    assert_eq!(
        problem.message,
        "The project runtime for this operation failed to publish; reopen the project"
    );
    sealed.harness.shutdown().await;
}

const MARKDOWN_OK: &str = "\
## health\\_read

### Payload

    {
      \"status\": \"ok\"
    }
";

const MARKDOWN_DEGRADED: &str = "\
## health\\_read

### Payload

    {
      \"status\": \"degraded\"
    }
";

async fn open_health_project() -> HealthProject {
    let isolation = test_temp_dir();
    let project_root = isolation.path().join("project");
    seed_project(&project_root);
    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        isolation.path(),
        [project_root.clone()],
    )
    .await
    .expect("production MCP composition");
    HealthProject {
        harness,
        project_root,
        isolation,
    }
}

fn seed_project(project_root: &Path) {
    fs::create_dir_all(project_root.join("src")).expect("project source directory");
    fs::write(project_root.join("src/lib.rs"), "pub fn marker() {}\n").expect("source file");
    let git = crate::common::git_program();
    let init = Command::new(&git)
        .args(["init", "-q"])
        .current_dir(project_root)
        .status()
        .expect("git init");
    assert!(init.success(), "git init must succeed");
    let add = Command::new(&git)
        .args(["add", "."])
        .current_dir(project_root)
        .status()
        .expect("git add");
    assert!(add.success(), "git add must succeed");
    let commit = Command::new(&git)
        .args([
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            "health read fixture",
        ])
        .current_dir(project_root)
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit must succeed");
}

async fn seal_serving_database(fixture: HealthProject) -> HealthProject {
    let HealthProject {
        harness,
        project_root,
        isolation,
    } = fixture;
    let database = {
        let server = harness
            .server(&project_root)
            .expect("mounted project server");
        server
            .cg()
            .await
            .db()
            .canonical_database_path()
            .to_path_buf()
    };
    harness.shutdown().await;
    let mut permissions = fs::metadata(&database)
        .expect("serving database metadata")
        .permissions();
    permissions.set_mode(0o444);
    fs::set_permissions(&database, permissions).expect("seal serving database");
    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        isolation.path(),
        [project_root.clone()],
    )
    .await
    .expect("read-only production MCP composition");
    HealthProject {
        harness,
        project_root,
        isolation,
    }
}

async fn serving_database_path(fixture: &HealthProject) -> PathBuf {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("mounted project server");
    server
        .cg()
        .await
        .db()
        .canonical_database_path()
        .to_path_buf()
}

async fn call_health(fixture: &HealthProject, arguments: Value) -> JsonRpcResponse {
    fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_health_read", arguments)
        .await
        .expect("production MCP tools/call tracedecay_health_read")
}

struct HealthProblem {
    schema_id: String,
    kind: String,
    code: String,
    retry: String,
    message: String,
}

async fn call_health_problem(fixture: &HealthProject, arguments: Value) -> HealthProblem {
    let response = call_health(fixture, arguments).await;
    assert!(
        response.error.is_none(),
        "a sealed serving database must stay a tool result, not a transport error: {:?}",
        response.error
    );
    let result = response.result.expect("health read tool result");
    assert_eq!(
        result["isError"],
        json!(true),
        "a sealed serving database must refuse the read: {result}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .expect("health read text");
    let envelope: Value = serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("health read refusal was not an application envelope: {error}; text={text}")
    });
    HealthProblem {
        schema_id: envelope["contract"]["schema_id"]
            .as_str()
            .expect("schema id")
            .to_owned(),
        kind: envelope["problem"]["kind"]
            .as_str()
            .expect("problem kind")
            .to_owned(),
        code: envelope["problem"]["code"]
            .as_str()
            .expect("problem code")
            .to_owned(),
        retry: envelope["problem"]["retry"]
            .as_str()
            .expect("problem retry")
            .to_owned(),
        message: envelope["problem"]["message"]
            .as_str()
            .expect("problem message")
            .to_owned(),
    }
}

async fn call_health_text(fixture: &HealthProject, arguments: Value) -> String {
    let response = call_health(fixture, arguments).await;
    assert!(
        response.error.is_none(),
        "health read must return a tool result, not a transport error: {:?}",
        response.error
    );
    let result = response.result.expect("health read tool result");
    assert_ne!(
        result["isError"],
        json!(true),
        "health read refused: {result}"
    );
    result["content"][0]["text"]
        .as_str()
        .expect("health read text")
        .to_owned()
}

async fn call_health_payload(fixture: &HealthProject, arguments: Value) -> Value {
    let text = call_health_text(fixture, arguments).await;
    let envelope: Value = serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!("health read JSON was not an application envelope: {error}; text={text}")
    });
    assert_eq!(
        envelope["contract"]["schema_id"],
        json!("schema.application.primitive.health-read.result"),
        "{envelope}"
    );
    assert_eq!(
        envelope["outcome"]["outcome"],
        json!("evidence"),
        "{envelope}"
    );
    envelope["outcome"]["value"]["payload"].clone()
}
