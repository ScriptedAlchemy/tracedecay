//! `tracedecay_project_list` as an MCP `tools/call`.
//!
//! The page is the profile registry the server was given, not a live git
//! scan. `summary` counts that page; `truncated` is the only signal that
//! more rows exist. The calling checkout is `is_active` when it is
//! registered. Credential-bearing remotes stay in the registry row and do
//! not appear in the payload.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay::project::TraceDecay;
use tracedecay::test_support::host_admission::{
    HostAdmissionTestRuntimeV1, ProjectScopedTestRuntimeV1,
};

use crate::support::{
    TestEnv, TestTempDir, extract_real_server_text, handle_real_server_tool_call,
    init_test_project, test_temp_dir,
};

const BETA_ID: &str = "proj_beta";
const SECRET_REMOTE: &str = "https://token:secret@example.test/beta.git";
const ALPHA_CREATED_AT: i64 = 1_700_000_001;
const ALPHA_LAST_SEEN_AT: i64 = 1_700_000_010;
const BETA_CREATED_AT: i64 = 1_700_000_002;
const BETA_LAST_SEEN_AT: i64 = 1_700_000_020;

struct HeldServer {
    server: Arc<McpServer>,
    registry_path: String,
    _env: TestEnv,
    _root: TestTempDir,
}

struct RegisteredProjects {
    held: HeldServer,
    alpha_id: String,
    alpha_path: String,
    alpha_git: String,
    beta_path: String,
    beta_root: PathBuf,
}

fn tool_json(result: &Value) -> Value {
    let text = extract_real_server_text(result);
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tracedecay_project_list text was not JSON ({error}): {text}")
    })
}

fn project_identity(cg: &TraceDecay) -> (String, tracedecay_domain::ProjectId) {
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("initialized project id");
    let typed =
        tracedecay_domain::ProjectId::new(project_id.clone()).expect("project id is well formed");
    (project_id, typed)
}

fn path_label(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .expect("registered path has a file name")
}

fn stamp_project(database: &Path, project_id: &str, created_at: i64, last_seen_at: i64) {
    let connection = rusqlite::Connection::open(database)
        .unwrap_or_else(|error| panic!("open registry '{}': {error}", database.display()));
    connection
        .busy_timeout(Duration::from_secs(5))
        .unwrap_or_else(|error| panic!("registry busy timeout: {error}"));
    let updated = connection
        .execute(
            "UPDATE code_projects
             SET created_at = ?1, last_seen_at = ?2
             WHERE project_id = ?3",
            rusqlite::params![created_at, last_seen_at, project_id],
        )
        .unwrap_or_else(|error| panic!("stamp {project_id}: {error}"));
    assert_eq!(updated, 1, "registry row for {project_id}");
}

async fn serve(graph: TraceDecay, runtime: ProjectScopedTestRuntimeV1) -> (Arc<McpServer>, String) {
    let registry_path = runtime
        .profile_database_lease()
        .db_path()
        .display()
        .to_string();
    let server = McpServer::new_with_host_admission_test_runtime_for_test(graph, None, runtime)
        .await
        .expect("MCP server");
    (server, registry_path)
}

async fn open_empty() -> HeldServer {
    let root = test_temp_dir();
    let project = root.path().join("caller");
    let profile = root.path().join("profile");
    fs::create_dir_all(&profile).expect("profile directory");
    let (mut cg, env) = init_test_project(&project).await;
    let (_, typed_id) = project_identity(&cg);
    let project_root = cg.project_root().to_path_buf();
    let runtime = HostAdmissionTestRuntimeV1::project_scoped(&profile, &project_root, typed_id)
        .await
        .expect("profile runtime");
    let (server, registry_path) = serve(cg.into_inner(), runtime).await;
    HeldServer {
        server,
        registry_path,
        _env: env,
        _root: root,
    }
}

async fn open_registered() -> RegisteredProjects {
    let root = test_temp_dir();
    let alpha = root.path().join("alpha-checkout");
    let beta = root.path().join("beta-checkout");
    let profile = root.path().join("profile");
    fs::create_dir_all(&beta).expect("sibling checkout");
    fs::create_dir_all(&profile).expect("profile directory");
    let (mut cg, env) = init_test_project(&alpha).await;
    let (alpha_id, typed_id) = project_identity(&cg);
    let alpha_root = cg.project_root().to_path_buf();
    let runtime = HostAdmissionTestRuntimeV1::project_scoped(&profile, &alpha_root, typed_id)
        .await
        .expect("profile runtime");
    let git_dir = alpha_root.join(".git");
    assert!(
        git_dir.is_dir(),
        "registration prep initializes the caller checkout"
    );
    let alpha_record = runtime
        .upsert_code_project(
            &alpha_id,
            &alpha_root,
            Some(git_dir.as_path()),
            None,
            Some("release"),
        )
        .await
        .expect("register the calling checkout");
    let beta_record = runtime
        .upsert_code_project(BETA_ID, &beta, None, Some(SECRET_REMOTE), Some("trunk"))
        .await
        .expect("register the sibling checkout");
    let database = runtime.profile_database_lease().db_path().to_path_buf();
    stamp_project(&database, &alpha_id, ALPHA_CREATED_AT, ALPHA_LAST_SEEN_AT);
    stamp_project(&database, BETA_ID, BETA_CREATED_AT, BETA_LAST_SEEN_AT);
    assert_eq!(alpha_record.display_root, alpha_record.canonical_root);
    assert_eq!(beta_record.display_root, beta_record.canonical_root);
    assert_eq!(path_label(&alpha_record.display_root), "alpha-checkout");
    assert_eq!(path_label(&beta_record.display_root), "beta-checkout");
    let alpha_git = alpha_record
        .git_common_dir
        .clone()
        .expect("caller git directory is stored");
    let (server, registry_path) = serve(cg.into_inner(), runtime).await;
    RegisteredProjects {
        held: HeldServer {
            server,
            registry_path,
            _env: env,
            _root: root,
        },
        alpha_id,
        alpha_path: alpha_record.display_root,
        alpha_git,
        beta_path: beta_record.display_root,
        beta_root: beta,
    }
}

fn public_project(
    project_id: &str,
    label: &str,
    path: &str,
    git_common_dir: Option<&str>,
    branch: &str,
    created_at: i64,
    last_seen_at: i64,
    is_active: bool,
) -> Value {
    json!({
        "project_id": project_id,
        "label": label,
        "project_root": path,
        "display_root": path,
        "canonical_root": path,
        "git_common_dir": git_common_dir,
        "default_branch": branch,
        "created_at": created_at,
        "last_seen_at": last_seen_at,
        "is_active": is_active,
    })
}

fn tree_project(
    project_id: &str,
    label: &str,
    path: &str,
    kind: &str,
    branch: &str,
    alias_count: usize,
    last_seen_at: i64,
    is_active: bool,
) -> Value {
    json!({
        "project_id": project_id,
        "label": label,
        "project_root": path,
        "canonical_root": path,
        "kind": kind,
        "default_branch": branch,
        "branches": [branch],
        "store_count": 0,
        "artifact_count": 0,
        "alias_count": alias_count,
        "last_seen_at": last_seen_at,
        "is_active": is_active,
    })
}

fn repo_group(label: &str, git_common_dir: Option<&str>, branch: &str, project: Value) -> Value {
    json!({
        "label": label,
        "git_common_dir": git_common_dir,
        "project_count": 1,
        "branches": [branch],
        "projects": [project],
    })
}

fn listing(
    registry_path: &str,
    limit: u64,
    truncated: bool,
    projects: Vec<Value>,
    project_tree: Vec<Value>,
) -> Value {
    json!({
        "status": "ok",
        "title": "registered projects",
        "registry_path": registry_path,
        "limit": limit,
        "truncated": truncated,
        "summary": {
            "project_count": projects.len(),
            "repo_count": project_tree.len(),
            "truncated": truncated,
        },
        "projects": projects,
        "project_tree": project_tree,
    })
}

impl RegisteredProjects {
    fn alpha_public(&self) -> Value {
        public_project(
            &self.alpha_id,
            "alpha-checkout",
            &self.alpha_path,
            Some(&self.alpha_git),
            "release",
            ALPHA_CREATED_AT,
            ALPHA_LAST_SEEN_AT,
            true,
        )
    }

    fn beta_public(&self) -> Value {
        public_project(
            BETA_ID,
            "beta-checkout",
            &self.beta_path,
            None,
            "trunk",
            BETA_CREATED_AT,
            BETA_LAST_SEEN_AT,
            false,
        )
    }

    fn alpha_group(&self) -> Value {
        repo_group(
            "alpha-checkout",
            Some(&self.alpha_git),
            "release",
            tree_project(
                &self.alpha_id,
                "alpha-checkout",
                &self.alpha_path,
                "primary",
                "release",
                2,
                ALPHA_LAST_SEEN_AT,
                true,
            ),
        )
    }

    fn beta_group(&self) -> Value {
        repo_group(
            "beta-checkout",
            None,
            "trunk",
            tree_project(
                BETA_ID,
                "beta-checkout",
                &self.beta_path,
                "project",
                "trunk",
                2,
                BETA_LAST_SEEN_AT,
                false,
            ),
        )
    }

    fn full_page(&self, limit: u64) -> Value {
        listing(
            &self.held.registry_path,
            limit,
            false,
            vec![self.beta_public(), self.alpha_public()],
            vec![self.alpha_group(), self.beta_group()],
        )
    }

    fn markdown(&self) -> String {
        format!(
            "Found 2 registered projects across 2 repositories.\n\n\
             Repositories:\n\
             - alpha-checkout (branches: release)\n\
               - `{}` * [primary] branches: release; stores: 0; path: {}\n\
             - beta-checkout (branches: trunk)\n\
               - `{BETA_ID}` [project] branches: trunk; stores: 0; path: {}\n",
            self.alpha_id, self.alpha_path, self.beta_path
        )
    }
}

#[tokio::test]
async fn project_list_reports_an_empty_registry() {
    let fixture = open_empty().await;

    let json_result = handle_real_server_tool_call(
        &fixture.server,
        "tracedecay_project_list",
        json!({"format": "json"}),
    )
    .await;
    assert_eq!(
        tool_json(&json_result),
        json!({
            "status": "ok",
            "title": "registered projects",
            "registry_path": fixture.registry_path,
            "limit": 25,
            "truncated": false,
            "summary": {
                "project_count": 0,
                "repo_count": 0,
                "truncated": false,
            },
            "project_tree": [],
            "projects": [],
        })
    );

    let markdown = handle_real_server_tool_call(
        &fixture.server,
        "tracedecay_project_list",
        json!({"format": "markdown"}),
    )
    .await;
    assert_eq!(
        extract_real_server_text(&markdown),
        "No registered projects found."
    );
}

#[tokio::test]
async fn project_list_pages_registered_projects_and_marks_the_caller_active() {
    let fixture = open_registered().await;

    let listed = handle_real_server_tool_call(
        &fixture.held.server,
        "tracedecay_project_list",
        json!({"format": "json"}),
    )
    .await;
    assert_eq!(tool_json(&listed), fixture.full_page(25));

    // A missing numeric limit uses the default page. A negative number is
    // not a limit, so it uses that same default rather than failing closed.
    let negative = handle_real_server_tool_call(
        &fixture.held.server,
        "tracedecay_project_list",
        json!({"limit": -1, "format": "json"}),
    )
    .await;
    assert_eq!(tool_json(&negative), fixture.full_page(25));

    // Zero is present but below the floor, so the page is one row: the
    // newest registration, with truncation set.
    let newest = handle_real_server_tool_call(
        &fixture.held.server,
        "tracedecay_project_list",
        json!({"limit": 0, "format": "json"}),
    )
    .await;
    assert_eq!(
        tool_json(&newest),
        listing(
            &fixture.held.registry_path,
            1,
            true,
            vec![fixture.beta_public()],
            vec![fixture.beta_group()],
        )
    );

    let clamped = handle_real_server_tool_call(
        &fixture.held.server,
        "tracedecay_project_list",
        json!({"limit": 1000, "format": "json"}),
    )
    .await;
    assert_eq!(tool_json(&clamped), fixture.full_page(100));

    let markdown = handle_real_server_tool_call(
        &fixture.held.server,
        "tracedecay_project_list",
        json!({"format": "markdown"}),
    )
    .await;
    assert_eq!(extract_real_server_text(&markdown), fixture.markdown());

    assert!(
        !fixture.beta_root.join(".tracedecay").exists(),
        "listing another checkout must not create its store"
    );
}
