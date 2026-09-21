//! `tracedecay_skill_view` as an MCP client sees it: one `tools/call`, one skill.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::mcp::McpServer;
use tracedecay_automation_runtime::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSupportFile,
    create_managed_skill, default_managed_skill_targets,
};
use tracedecay_automation_runtime::automation::skill_usage::load_skill_usage_record;
use tracedecay_runtime_core::storage::default_profile_root;

use crate::fixture;
use crate::mcp_server_test::support::{
    jsonrpc_request, response_with_id, run_client_connection_with_messages,
};
use crate::support::{
    GlobalDbEnvGuard, HomeEnvGuard, ProcessEnvGuard, TestTraceDecay, lock_process_env,
    open_active_project_scoped_runtime,
};

const PROBE_ID: &str = "probe-skill";
const OTHER_ID: &str = "other-skill";
const PROBE_BODY: &str = "Read the checklist, then stop.";
const OTHER_BODY: &str = "Leave the other skill unread.";
const SUPPORT_BODY: &str = "alpha\nbeta\n";
const PROBE_CHECKSUM: &str =
    "sha256:5fc07170419d9a68b72f1e318d73315c4763985e42b296aad6bb0fc18d68c719";
const PROBE_MARKDOWN: &str = "\
## Managed Skill: probe-skill
**status:** ok
**title:** Probe Skill
**state:** active
**category:** maintenance
**checksum:** sha256:5fc07170419d9a68b72f1e318d73315c4763985e42b296aad6bb0fc18d68c719
**targets:** cursor, codex, claude, agents, opencode, kimi, kiro, hermes
**support_files_included:** false

### Summary
Read the probe skill before editing.

### Body
Read the checklist, then stop.

### Support Files
- **references/checklist.md** - 11 bytes (pass include_support_files=true only for a required body)
";

struct SkillViewServer {
    server: Arc<McpServer>,
    profile_root: PathBuf,
    _dir: TempDir,
    _home_guard: HomeEnvGuard,
    _global_db_guard: GlobalDbEnvGuard,
    _env_lock: ProcessEnvGuard,
}

async fn open_skill_view_server() -> SkillViewServer {
    let env_lock = lock_process_env().await;
    let dir = TempDir::new().expect("skill view temp dir");
    let project = dir.path().join("repo");
    std::fs::create_dir_all(project.join("src")).expect("fixture source dir");
    std::fs::write(project.join("src/lib.rs"), "pub fn fixture() {}\n").expect("fixture source");
    let home = dir.path().join("home");
    let home_guard = HomeEnvGuard::set(&env_lock, &home);
    let global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));
    let graph = TestTraceDecay::new(
        fixture::init_project_from_template(&project)
            .await
            .expect("initialized skill-view project"),
    );
    let profile_root = default_profile_root().expect("isolated profile root");
    let runtime = open_active_project_scoped_runtime(&graph).await;
    let server =
        McpServer::new_with_host_admission_test_runtime_for_test(graph.into_inner(), None, runtime)
            .await
            .expect("registered skill-view MCP server");
    SkillViewServer {
        server,
        profile_root,
        _dir: dir,
        _home_guard: home_guard,
        _global_db_guard: global_db_guard,
        _env_lock: env_lock,
    }
}

fn probe_draft() -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: PROBE_ID.to_string(),
        title: "Probe Skill".to_string(),
        summary: "Read the probe skill before editing.".to_string(),
        routing_description: "Use when proving skill view.".to_string(),
        category: "maintenance".to_string(),
        targets: default_managed_skill_targets(),
        body_markdown: PROBE_BODY.to_string(),
        support_files: vec![
            ManagedSupportFile::new("references/checklist.md", SUPPORT_BODY.as_bytes().to_vec())
                .expect("probe support file"),
        ],
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: "probe-author".to_string(),
            run_id: Some("run-probe".to_string()),
        },
    }
}

fn other_draft() -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: OTHER_ID.to_string(),
        title: "Other Skill".to_string(),
        summary: "Not the probe.".to_string(),
        routing_description: "Use when the probe is the wrong skill.".to_string(),
        category: "maintenance".to_string(),
        targets: default_managed_skill_targets(),
        body_markdown: OTHER_BODY.to_string(),
        support_files: Vec::new(),
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::User,
            actor: "other-author".to_string(),
            run_id: None,
        },
    }
}

async fn call_skill_view(server: &Arc<McpServer>, id: i64, arguments: Value) -> Value {
    let request = jsonrpc_request(
        json!(id),
        "tools/call",
        json!({
            "name": "tracedecay_skill_view",
            "arguments": arguments,
        }),
    );
    let responses = run_client_connection_with_messages(Arc::clone(server), vec![request]).await;
    response_with_id(&responses, json!(id))
}

fn successful_text<'a>(response: &'a Value) -> &'a str {
    assert!(
        response.get("error").is_none(),
        "tracedecay_skill_view failed: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tracedecay_skill_view returned no text: {response}"))
}

fn successful_json(response: &Value) -> Value {
    let text = successful_text(response);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tracedecay_skill_view text was not JSON: {error}\n{text}"))
}

fn stable_evidence(recommendation: &Value) -> Vec<String> {
    recommendation["evidence"]
        .as_array()
        .unwrap_or_else(|| panic!("recommendation evidence missing: {recommendation}"))
        .iter()
        .filter_map(Value::as_str)
        .filter(|entry| {
            !entry.starts_with("last_activity_at=") && !entry.starts_with("activated_at=")
        })
        .map(str::to_owned)
        .collect()
}

fn support_file_text(skill: &Value) -> String {
    let bytes = skill["support_files"][0]["bytes"]
        .as_array()
        .unwrap_or_else(|| panic!("included support file has no bytes: {skill}"))
        .iter()
        .map(|byte| {
            u8::try_from(byte.as_u64().expect("support byte")).expect("support byte fits in u8")
        })
        .collect::<Vec<_>>();
    String::from_utf8(bytes).expect("support file is utf-8")
}

#[tokio::test]
async fn skill_view_returns_the_requested_package_and_withholds_support_bytes() {
    let fixture = open_skill_view_server().await;
    create_managed_skill(&fixture.profile_root, probe_draft())
        .await
        .expect("probe skill");
    create_managed_skill(&fixture.profile_root, other_draft())
        .await
        .expect("other skill");
    let profile_root = fixture
        .profile_root
        .to_str()
        .expect("profile root is utf-8")
        .to_string();

    let summary = call_skill_view(
        &fixture.server,
        1,
        json!({"id": PROBE_ID, "format": "json"}),
    )
    .await;
    let summary_text = successful_text(&summary);
    assert!(
        !summary_text.contains(SUPPORT_BODY),
        "default view must not inline support-file bytes: {summary_text}"
    );
    assert!(
        !summary_text.contains(OTHER_BODY),
        "probe view must not return the other skill: {summary_text}"
    );
    let summary = successful_json(&summary);
    assert_eq!(summary["status"], "ok");
    assert_eq!(summary["profile_root"], profile_root);
    assert_eq!(summary["support_files_included"], false);
    assert_eq!(summary["skill"]["metadata"]["id"], PROBE_ID);
    assert_eq!(summary["skill"]["metadata"]["title"], "Probe Skill");
    assert_eq!(summary["skill"]["metadata"]["state"], "active");
    assert_eq!(summary["skill"]["metadata"]["category"], "maintenance");
    assert_eq!(
        summary["skill"]["metadata"]["summary"],
        "Read the probe skill before editing."
    );
    assert_eq!(
        summary["skill"]["metadata"]["routing_description"],
        "Use when proving skill view."
    );
    assert_eq!(summary["skill"]["metadata"]["checksum"], PROBE_CHECKSUM);
    assert_eq!(summary["skill"]["metadata"]["pinned"], false);
    assert_eq!(
        summary["skill"]["metadata"]["targets"],
        json!([
            "cursor", "codex", "claude", "agents", "opencode", "kimi", "kiro", "hermes"
        ])
    );
    assert_eq!(
        summary["skill"]["metadata"]["provenance"],
        json!({
            "source": "automation_run",
            "actor": "probe-author",
            "run_id": "run-probe"
        })
    );
    assert_eq!(summary["skill"]["body_markdown"], PROBE_BODY);
    assert_eq!(summary["skill"]["support_files"], json!([]));
    assert_eq!(
        summary["support_file_summaries"],
        json!([{
            "path": "references/checklist.md",
            "byte_len": 11
        }])
    );
    assert_eq!(summary["usage_summary"]["skill_id"], PROBE_ID);
    assert_eq!(summary["usage_summary"]["view_count"], 1);
    assert_eq!(summary["usage_summary"]["use_count"], 0);
    assert_eq!(summary["usage_summary"]["patch_count"], 0);
    assert_eq!(
        summary["usage_summary"]["targets"],
        json!([
            "agents.md",
            "claude",
            "codex",
            "cursor",
            "hermes",
            "kimi",
            "kiro",
            "mcp",
            "opencode"
        ])
    );
    assert_eq!(summary["stale_recommendation"]["skill_id"], PROBE_ID);
    assert_eq!(summary["stale_recommendation"]["stale"], false);
    assert_eq!(summary["stale_recommendation"]["recommendation"], "keep");
    assert_eq!(
        summary["stale_recommendation"]["reason"],
        "recent or meaningful activity is present"
    );
    assert_eq!(
        stable_evidence(&summary["stale_recommendation"]),
        vec![
            "state=active".to_string(),
            "pinned=false".to_string(),
            "views=1".to_string(),
            "uses=0".to_string(),
            "patches=0".to_string(),
            "created_by=probe-author".to_string(),
            "provenance_source=automation_run".to_string(),
        ]
    );
    assert_eq!(summary["improvement_recommendation"]["skill_id"], PROBE_ID);
    assert_eq!(summary["improvement_recommendation"]["improvement"], false);
    assert_eq!(
        summary["improvement_recommendation"]["recommendation"],
        "none"
    );
    assert_eq!(summary["improvement_recommendation"]["priority"], "none");
    assert_eq!(
        summary["improvement_recommendation"]["reason"],
        "no repeated correction or failed-use signal is present"
    );

    let included = call_skill_view(
        &fixture.server,
        2,
        json!({
            "id": PROBE_ID,
            "format": "json",
            "include_support_files": true,
        }),
    )
    .await;
    let included = successful_json(&included);
    assert_eq!(included["support_files_included"], true);
    assert_eq!(included["skill"]["body_markdown"], PROBE_BODY);
    assert_eq!(included["usage_summary"]["view_count"], 2);
    assert_eq!(included["usage_summary"]["use_count"], 0);
    assert_eq!(
        included["skill"]["support_files"][0]["path"],
        "references/checklist.md"
    );
    assert_eq!(support_file_text(&included["skill"]), SUPPORT_BODY);
    assert_eq!(
        included["support_file_summaries"],
        json!([{
            "path": "references/checklist.md",
            "byte_len": 11
        }])
    );

    let markdown = call_skill_view(&fixture.server, 3, json!({"id": PROBE_ID})).await;
    assert_eq!(successful_text(&markdown), PROBE_MARKDOWN);

    let other = call_skill_view(
        &fixture.server,
        4,
        json!({"id": OTHER_ID, "format": "json"}),
    )
    .await;
    let other = successful_json(&other);
    assert_eq!(other["skill"]["metadata"]["id"], OTHER_ID);
    assert_eq!(other["skill"]["body_markdown"], OTHER_BODY);
    assert_eq!(other["skill"]["support_files"], json!([]));
    assert_eq!(other["support_file_summaries"], json!([]));
    assert_eq!(other["usage_summary"]["view_count"], 1);
    assert_eq!(other["support_files_included"], false);
    assert_ne!(other["skill"]["body_markdown"], PROBE_BODY);

    let probe_usage = load_skill_usage_record(&fixture.profile_root, PROBE_ID)
        .await
        .expect("probe usage ledger")
        .expect("probe view should persist usage");
    let other_usage = load_skill_usage_record(&fixture.profile_root, OTHER_ID)
        .await
        .expect("other usage ledger")
        .expect("other view should persist usage");
    assert_eq!(probe_usage.view_count, 3);
    assert_eq!(probe_usage.use_count, 0);
    assert_eq!(other_usage.view_count, 1);
    assert_eq!(other_usage.use_count, 0);
}

#[tokio::test]
async fn skill_view_denies_missing_id_and_unknown_skill() {
    let fixture = open_skill_view_server().await;

    let missing = call_skill_view(&fixture.server, 11, json!({})).await;
    assert_eq!(missing["jsonrpc"], "2.0");
    assert_eq!(missing["id"], 11);
    assert_eq!(
        missing["error"],
        json!({
            "code": -32602,
            "message": "missing required parameter: id",
            "data": {
                "tool": "tracedecay_skill_view",
                "reason_code": "missing_required_parameter",
                "retryable": false,
                "detail": "missing required parameter: id"
            }
        })
    );
    assert!(missing.get("result").is_none());

    let unknown = call_skill_view(&fixture.server, 12, json!({"id": "no-such-skill"})).await;
    assert_eq!(unknown["jsonrpc"], "2.0");
    assert_eq!(unknown["id"], 12);
    assert_eq!(
        unknown["error"],
        json!({
            "code": -32602,
            "message": "managed skill 'no-such-skill' not found",
            "data": {
                "tool": "tracedecay_skill_view",
                "reason_code": "not_found",
                "retryable": false,
                "detail": "managed skill 'no-such-skill' not found"
            }
        })
    );
    assert!(unknown.get("result").is_none());
}
