//! Host-visible behavior of `tracedecay_hermes_skill_bridge`.
//!
//! Calls go through the MCP `tools/call` path. The install lives under an
//! isolated `HOME`; `HERMES_HOME` is pointed at a different tree so a bridge
//! that honored an alternate root would return the wrong skills.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
#[cfg(unix)]
use std::os::unix::fs::symlink;
use tempfile::TempDir;
use tracedecay::mcp::McpServer;

use crate::mcp_server_test::support::{
    jsonrpc_request, response_with_id, run_client_connection_with_messages, successful_tool_text,
};
use crate::support::{TestEnv, TestTraceDecay, canonicalize_test_dir, init_test_project};

const WORKFLOW_BODY: &str =
    "---\nname: workflow\ndescription: Reusable workflow\n---\n\nDo the work.\n";
const BARE_NOTE_BODY: &str = "# just a note\n";
const SECRET_BODY: &str = "---\nname: secret\ndescription: Not in the standard install\n---\n";

struct HermesHomeGuard {
    previous: Option<OsString>,
}

impl HermesHomeGuard {
    fn set(path: &Path) -> Self {
        let previous = std::env::var_os("HERMES_HOME");
        unsafe {
            std::env::set_var("HERMES_HOME", path);
        }
        Self { previous }
    }
}

impl Drop for HermesHomeGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var("HERMES_HOME", value),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }
}

struct IsolatedHome {
    home: PathBuf,
    _env: TestEnv,
    _dir: TempDir,
}

async fn open_isolated_home() -> (IsolatedHome, TestTraceDecay) {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    let (cg, env) = init_test_project(&project).await;
    let home = canonicalize_test_dir(&project.join("home"));
    (
        IsolatedHome {
            home,
            _env: env,
            _dir: dir,
        },
        cg,
    )
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn write_skill(dir: &Path, body: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("SKILL.md"), body).unwrap();
}

fn snapshot_roots(roots: &[PathBuf]) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    for root in roots {
        files.extend(file_snapshot(root));
    }
    files
}

fn file_snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    if !root.exists() {
        return files;
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).unwrap();
                files.insert(path, target.to_string_lossy().into_owned().into_bytes());
            } else if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let bytes = fs::read(&path).unwrap();
                files.insert(path, bytes);
            }
        }
    }
    files
}

async fn open_server(cg: TestTraceDecay) -> Arc<McpServer> {
    Box::pin(McpServer::new(cg.into_inner(), None)).await
}

async fn call_bridge(server: &Arc<McpServer>, id: i64, arguments: Value) -> Value {
    let responses = run_client_connection_with_messages(
        Arc::clone(server),
        vec![jsonrpc_request(
            json!(id),
            "tools/call",
            json!({
                "name": "tracedecay_hermes_skill_bridge",
                "arguments": arguments,
            }),
        )],
    )
    .await;
    response_with_id(&responses, json!(id))
}

fn json_payload(response: &Value) -> Value {
    let text = successful_tool_text(response, "tracedecay_hermes_skill_bridge");
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tracedecay_hermes_skill_bridge JSON was not an object: {error}\n{text}")
    })
}

fn assert_markdown_line(markdown: &str, line: &str) {
    assert!(
        markdown.lines().any(|candidate| candidate == line),
        "missing markdown line {line:?}\n{markdown}"
    );
}

fn contracts() -> Value {
    json!({
        "lifecycle_owner": "hermes",
        "mutation_policy": "read_only; use Hermes to mutate Hermes-owned skills",
        "discovery_policy": "standard_user_install_only"
    })
}

fn skill_summary(
    name: &str,
    path: &Path,
    category: Option<&str>,
    description: Option<&str>,
    body: Option<&str>,
    usage: Option<Value>,
    pending_write_ids: &[&str],
) -> Value {
    let mut skill = json!({
        "name": name,
        "path": path_str(path),
        "pending_write_ids": pending_write_ids,
    });
    if let Some(category) = category {
        skill["category"] = json!(category);
    }
    if let Some(description) = description {
        skill["description"] = json!(description);
    }
    if let Some(body) = body {
        skill["body_markdown"] = json!(body);
    }
    if let Some(usage) = usage {
        skill["usage"] = usage;
    }
    skill
}

fn pending_write(
    id: &str,
    source: &Path,
    action: Option<&str>,
    name: Option<&str>,
    summary: Option<&str>,
    origin: Option<&str>,
    created_at: Option<&str>,
    payload: Option<Value>,
) -> Value {
    let mut pending = json!({
        "id": id,
        "source_path": path_str(source),
    });
    if let Some(action) = action {
        pending["action"] = json!(action);
    }
    if let Some(name) = name {
        pending["name"] = json!(name);
    }
    if let Some(summary) = summary {
        pending["summary"] = json!(summary);
    }
    if let Some(origin) = origin {
        pending["origin"] = json!(origin);
    }
    if let Some(created_at) = created_at {
        pending["created_at"] = json!(created_at);
    }
    if let Some(payload) = payload {
        pending["payload"] = payload;
    }
    pending
}

fn populated_inventory(home: &Path, include_bodies: bool, include_payloads: bool) -> Value {
    let agent_home = home.join(".hermes");
    let skills_dir = agent_home.join("skills");
    let workflow_dir = skills_dir.join("ops").join("workflow");
    let bare_dir = skills_dir.join("bare-note");
    let staged = agent_home
        .join("pending")
        .join("skills")
        .join("staged.json");
    let other = agent_home.join("pending").join("skills").join("other.json");
    let workflow_payload = json!({"name": "workflow", "body": "draft"});
    let other_payload = json!({"name": "missing-skill", "body": "other"});
    json!({
        "status": "ok",
        "bridge": {
            "agent_home": path_str(&agent_home),
            "skills_dir": path_str(&skills_dir),
            "skill_count": 2,
            "pending_skill_count": 2,
            "pending_skill_corrupt_count": 1,
            "usage_record_count": 2,
            "archive_count": 1,
            "skills": [
                skill_summary(
                    "bare-note",
                    &bare_dir,
                    None,
                    None,
                    include_bodies.then_some(BARE_NOTE_BODY),
                    None,
                    &[],
                ),
                skill_summary(
                    "workflow",
                    &workflow_dir,
                    Some("ops"),
                    Some("Reusable workflow"),
                    include_bodies.then_some(WORKFLOW_BODY),
                    Some(json!({"uses": 3})),
                    &["approval-9"],
                ),
            ],
            "pending_skills": [
                pending_write(
                    "approval-9",
                    &staged,
                    Some("stage"),
                    Some("workflow"),
                    Some("revise the workflow steps"),
                    Some("operator"),
                    Some("2026-04-01T00:00:00Z"),
                    include_payloads.then_some(workflow_payload),
                ),
                pending_write(
                    "approval-other",
                    &other,
                    None,
                    Some("missing-skill"),
                    None,
                    None,
                    None,
                    include_payloads.then_some(other_payload),
                ),
            ],
            "usage_records": {
                "orphan": {"uses": 1},
                "workflow": {"uses": 3}
            },
            "contracts": contracts(),
        }
    })
}

fn seed_populated_install(home: &Path) -> PathBuf {
    let agent_home = home.join(".hermes");
    let skills_dir = agent_home.join("skills");
    write_skill(&skills_dir.join("ops").join("workflow"), WORKFLOW_BODY);
    write_skill(&skills_dir.join("bare-note"), BARE_NOTE_BODY);
    fs::write(
        skills_dir.join(".usage.json"),
        r#"{"orphan":{"uses":1},"workflow":{"uses":3}}"#,
    )
    .unwrap();
    fs::create_dir_all(skills_dir.join(".archive").join("retired")).unwrap();
    fs::write(skills_dir.join(".archive").join(".keep"), "hidden").unwrap();
    let pending = agent_home.join("pending/skills");
    fs::create_dir_all(&pending).unwrap();
    fs::write(
        pending.join("staged.json"),
        r#"{"id":"approval-9","action":"stage","summary":"revise the workflow steps","origin":"operator","created_at":"2026-04-01T00:00:00Z","payload":{"name":"workflow","body":"draft"}}"#,
    )
    .unwrap();
    fs::write(
        pending.join("other.json"),
        r#"{"id":"approval-other","payload":{"name":"missing-skill","body":"other"}}"#,
    )
    .unwrap();
    fs::write(pending.join("broken.json"), "not json").unwrap();
    fs::write(pending.join("notes.txt"), "not a pending write").unwrap();
    #[cfg(unix)]
    {
        let outside = home.join("outside-skill");
        write_skill(&outside, "---\nname: escaped\n---\n");
        symlink(&outside, skills_dir.join("escaped")).unwrap();
    }

    let alternate = home.join("custom-hermes");
    write_skill(&alternate.join("skills").join("secret"), SECRET_BODY);
    alternate
}

#[tokio::test]
async fn hermes_skill_bridge_mcp_returns_standard_install_inventory() {
    let (isolated, cg) = open_isolated_home().await;
    let alternate = seed_populated_install(&isolated.home);
    let _hermes_home = HermesHomeGuard::set(&alternate);
    let before = snapshot_roots(&[
        isolated.home.join(".hermes"),
        alternate.clone(),
        isolated.home.join("outside-skill"),
    ]);
    let server = open_server(cg).await;

    let markdown = call_bridge(&server, 1, json!({})).await;
    let markdown = successful_tool_text(&markdown, "tracedecay_hermes_skill_bridge");
    assert_markdown_line(markdown, "**status:** ok");
    assert_markdown_line(markdown, "**skill_count:** 2");
    assert_markdown_line(markdown, "**pending_skill_count:** 2");
    assert_markdown_line(markdown, "**pending_skill_corrupt_count:** 1");
    assert_markdown_line(markdown, "**usage_record_count:** 2");
    assert_markdown_line(markdown, "**archive_count:** 1");
    assert_markdown_line(markdown, "- **bare-note**");
    assert_markdown_line(markdown, "- **workflow**");
    assert_markdown_line(markdown, "  **category:** ops");
    assert_markdown_line(markdown, "  **description:** Reusable workflow");
    assert!(
        !markdown.contains("secret")
            && !markdown.contains("escaped")
            && !markdown.contains("Do the work."),
        "default inventory must omit alternate roots, symlink escapes, and skill bodies:\n{markdown}"
    );

    let omitted = json_payload(
        &call_bridge(
            &server,
            2,
            json!({"format": "json", "include_skill_bodies": false, "include_pending_payloads": false}),
        )
        .await,
    );
    let non_bool = json_payload(
        &call_bridge(
            &server,
            3,
            json!({"format": "json", "include_skill_bodies": "true", "include_pending_payloads": 1}),
        )
        .await,
    );
    let included = json_payload(
        &call_bridge(
            &server,
            4,
            json!({"format": "json", "include_skill_bodies": true, "include_pending_payloads": true}),
        )
        .await,
    );

    assert_eq!(omitted, populated_inventory(&isolated.home, false, false));
    assert_eq!(non_bool, populated_inventory(&isolated.home, false, false));
    assert_eq!(included, populated_inventory(&isolated.home, true, true));
    assert_eq!(
        included["bridge"]["skills"][1]["body_markdown"],
        WORKFLOW_BODY
    );
    assert_eq!(
        included["bridge"]["pending_skills"][0]["payload"],
        json!({"body": "draft", "name": "workflow"})
    );
    assert!(
        omitted["bridge"]["skills"][1]
            .get("body_markdown")
            .is_none()
    );
    assert!(
        omitted["bridge"]["pending_skills"][0]
            .get("payload")
            .is_none()
    );
    assert_eq!(
        snapshot_roots(&[
            isolated.home.join(".hermes"),
            alternate,
            isolated.home.join("outside-skill"),
        ]),
        before
    );
}

#[tokio::test]
async fn hermes_skill_bridge_mcp_reports_missing_install_as_empty_inventory() {
    let (isolated, cg) = open_isolated_home().await;
    let alternate = isolated.home.join("custom-hermes");
    write_skill(&alternate.join("skills").join("secret"), SECRET_BODY);
    let _hermes_home = HermesHomeGuard::set(&alternate);
    let before = snapshot_roots(std::slice::from_ref(&alternate));
    let server = open_server(cg).await;

    let response = call_bridge(&server, 7, json!({"format": "json"})).await;
    let payload = json_payload(&response);
    let agent_home = isolated.home.join(".hermes");
    assert_eq!(
        payload,
        json!({
            "status": "ok",
            "bridge": {
                "agent_home": path_str(&agent_home),
                "skills_dir": path_str(&agent_home.join("skills")),
                "skill_count": 0,
                "pending_skill_count": 0,
                "pending_skill_corrupt_count": 0,
                "usage_record_count": 0,
                "archive_count": 0,
                "skills": [],
                "pending_skills": [],
                "usage_records": {},
                "contracts": contracts(),
            }
        })
    );
    assert_eq!(snapshot_roots(&[alternate]), before);
    assert!(!agent_home.exists());
}

#[tokio::test]
async fn hermes_skill_bridge_mcp_rejects_invalid_usage_json() {
    let (isolated, cg) = open_isolated_home().await;
    let skills_dir = isolated.home.join(".hermes").join("skills");
    write_skill(&skills_dir.join("workflow"), WORKFLOW_BODY);
    let usage_path = skills_dir.join(".usage.json");
    fs::write(&usage_path, "not json").unwrap();
    let before = fs::read(&usage_path).unwrap();
    let skill_before = fs::read(skills_dir.join("workflow").join("SKILL.md")).unwrap();
    let server = open_server(cg).await;

    let response = call_bridge(&server, 9, json!({"format": "json"})).await;
    assert!(response.get("result").is_none() || response["result"].is_null());
    assert_eq!(response["error"]["code"], -32603);
    assert_eq!(
        response["error"]["data"]["tool"],
        "tracedecay_hermes_skill_bridge"
    );
    assert_eq!(
        response["error"]["message"],
        format!(
            "tool execution failed: config error: Hermes skill usage '{}' is invalid JSON: expected ident at line 1 column 2",
            usage_path.display()
        )
    );
    assert_eq!(fs::read(&usage_path).unwrap(), before);
    assert_eq!(
        fs::read(skills_dir.join("workflow").join("SKILL.md")).unwrap(),
        skill_before
    );
}
