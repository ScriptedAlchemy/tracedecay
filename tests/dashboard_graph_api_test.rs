use std::ffi::OsString;
use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use tokensave::dashboard;
use tokensave::tokensave::TokenSave;
use tokensave::types::{Edge, EdgeKind, FileRecord, Node, NodeKind, Visibility};

static GLOBAL_DB_ENV_LOCK: Mutex<()> = Mutex::new(());
const GLOBAL_DB_ENV: &str = "TOKENSAVE_GLOBAL_DB";

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

struct DashboardFixture {
    _tmp: TempDir,
    _env_guard: EnvVarGuard,
    base_url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for DashboardFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn tempdir_or_panic() -> TempDir {
    match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("failed to create temp dir: {err}"),
    }
}

fn create_runtime() -> tokio::runtime::Runtime {
    match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => panic!("failed to create tokio runtime: {err}"),
    }
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            panic!("failed to create {}: {err}", parent.display());
        }
    }
    if let Err(err) = fs::write(path, content) {
        panic!("failed to write {}: {err}", path.display());
    }
}

fn make_node(id: &str, kind: NodeKind, name: &str, file_path: &str, start_line: u32) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: format!("crate::dashboard::{name}"),
        file_path: file_path.to_string(),
        start_line,
        attrs_start_line: start_line,
        end_line: start_line + 4,
        start_column: 0,
        end_column: 1,
        signature: Some(format!("fn {name}()")),
        docstring: Some(format!("Fixture documentation for {name}")),
        visibility: Visibility::Pub,
        is_async: false,
        branches: 1,
        loops: 0,
        returns: 1,
        max_nesting: 1,
        unsafe_blocks: 0,
        unchecked_calls: 0,
        assertions: 0,
        updated_at: 1_700_000_000,
        parent_id: None,
    }
}

async fn setup_project(project_root: &Path) -> TokenSave {
    write_file(
        &project_root.join("src/dashboard/mod.rs"),
        "pub fn dashboard() {}\npub fn route_graph() {}\npub fn render_graph() {}\n",
    );
    match TokenSave::init(project_root).await {
        Ok(cg) => cg,
        Err(err) => panic!("failed to initialize tokensave fixture project: {err}"),
    }
}

async fn seed_graph_fixture(cg: &TokenSave) {
    let db = cg.db();
    let nodes = [
        make_node(
            "n-dashboard",
            NodeKind::Function,
            "dashboard",
            "src/dashboard/mod.rs",
            1,
        ),
        make_node(
            "n-route",
            NodeKind::Function,
            "route_graph",
            "src/dashboard/mod.rs",
            8,
        ),
        make_node(
            "n-render",
            NodeKind::Function,
            "render_graph",
            "src/dashboard/view.tsx",
            3,
        ),
        make_node(
            "n-state",
            NodeKind::Struct,
            "GraphState",
            "src/dashboard/mod.rs",
            20,
        ),
    ];
    if let Err(err) = db.insert_nodes(&nodes).await {
        panic!("failed to seed graph nodes: {err}");
    }

    let edges = [
        Edge {
            source: "n-dashboard".to_string(),
            target: "n-route".to_string(),
            kind: EdgeKind::Calls,
            line: Some(2),
        },
        Edge {
            source: "n-route".to_string(),
            target: "n-render".to_string(),
            kind: EdgeKind::Calls,
            line: Some(9),
        },
        Edge {
            source: "n-route".to_string(),
            target: "n-state".to_string(),
            kind: EdgeKind::Uses,
            line: Some(12),
        },
    ];
    if let Err(err) = db.insert_edges(&edges).await {
        panic!("failed to seed graph edges: {err}");
    }

    let files = [
        FileRecord {
            path: "src/dashboard/mod.rs".to_string(),
            content_hash: "hash-rust".to_string(),
            size: 128,
            modified_at: 1_700_000_000,
            indexed_at: 1_700_000_010,
            node_count: 3,
        },
        FileRecord {
            path: "src/dashboard/view.tsx".to_string(),
            content_hash: "hash-tsx".to_string(),
            size: 96,
            modified_at: 1_700_000_000,
            indexed_at: 1_700_000_010,
            node_count: 1,
        },
    ];
    if let Err(err) = db.upsert_files(&files).await {
        panic!("failed to seed graph files: {err}");
    }
}

fn pick_free_port() -> u16 {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) => panic!("failed to bind free local port: {err}"),
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(err) => panic!("failed to read bound local address: {err}"),
    };
    drop(listener);
    port
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(4)))
        .build()
        .into()
}

fn response_to_json(mut response: ureq::http::Response<ureq::Body>) -> (u16, Value) {
    let status = response.status().as_u16();
    let body = match response.body_mut().read_to_string() {
        Ok(body) => body,
        Err(err) => panic!("failed to read response body: {err}"),
    };
    let parsed = match serde_json::from_str::<Value>(&body) {
        Ok(value) => value,
        Err(err) => panic!("failed to decode JSON body `{body}`: {err}"),
    };
    (status, parsed)
}

fn get_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    let response = match agent.get(url).call() {
        Ok(response) => response,
        Err(err) => panic!("GET {url} failed: {err}"),
    };
    response_to_json(response)
}

async fn wait_for_dashboard(agent: &ureq::Agent, base_url: &str) {
    let probe = format!("{base_url}/api/capabilities");
    for _ in 0..80 {
        if agent.get(&probe).call().is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("dashboard server did not become ready at {base_url}");
}

async fn start_dashboard_fixture() -> DashboardFixture {
    let tmp = tempdir_or_panic();
    let project_root = tmp.path().join("project");
    let global_db_path = tmp.path().join("global").join("global.db");
    let env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);

    let cg = setup_project(&project_root).await;
    seed_graph_fixture(&cg).await;

    let port = pick_free_port();
    let base_url = format!("http://127.0.0.1:{port}");
    let server = tokio::spawn(async move {
        let _ = dashboard::run(&cg, "127.0.0.1", port, false).await;
    });

    let agent = http_agent();
    wait_for_dashboard(&agent, &base_url).await;

    DashboardFixture {
        _tmp: tmp,
        _env_guard: env_guard,
        base_url,
        server,
    }
}

#[test]
fn graph_api_returns_seeded_overview_search_detail_and_subgraph() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture().await;
        let agent = http_agent();

        let (status, capabilities) =
            get_json(&agent, &format!("{}/api/capabilities", fixture.base_url));
        assert_eq!(status, 200);
        assert_eq!(capabilities["features"]["graph"], true);
        assert!(
            capabilities["dashboards"]
                .as_array()
                .is_some_and(|dashboards| dashboards.iter().any(|name| name == "graph")),
            "capabilities should advertise the graph dashboard"
        );

        let (status, overview) = get_json(
            &agent,
            &format!("{}/api/plugins/graph/overview", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["totals"]["nodes"], 4);
        assert_eq!(overview["totals"]["edges"], 3);
        assert_eq!(overview["totals"]["files"], 2);
        assert!(
            overview["nodes_by_kind"].as_array().is_some_and(|rows| rows
                .iter()
                .any(|row| row["kind"] == "function" && row["count"] == 3)),
            "overview should include node counts by kind"
        );
        assert!(
            overview["files_by_language"]
                .as_array()
                .is_some_and(|rows| rows
                    .iter()
                    .any(|row| row["language"] == "rust" && row["count"] == 1)),
            "overview should include file counts by language"
        );

        let (status, search) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/graph/search?q=dashboard&limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(search["query"], "dashboard");
        assert!(
            search["results"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == "n-dashboard")),
            "search should include the exact dashboard symbol"
        );

        let (status, node) = get_json(
            &agent,
            &format!("{}/api/plugins/graph/node/n-route", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(
            node["node"]["qualified_name"],
            "crate::dashboard::route_graph"
        );
        assert_eq!(node["node"]["span"]["start_line"], 8);
        assert_eq!(node["node"]["doc"], "Fixture documentation for route_graph");

        let (status, neighbors) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/graph/node/n-route/neighbors",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(neighbors["node_id"], "n-route");
        assert!(
            neighbors["callers"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == "n-dashboard")),
            "neighbors should include callers"
        );
        assert!(
            neighbors["callees"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == "n-render")),
            "neighbors should include callees"
        );
        assert!(
            neighbors["edges_by_kind"]
                .as_array()
                .is_some_and(|rows| rows
                    .iter()
                    .any(|row| row["kind"] == "uses" && row["count"] == 1)),
            "neighbors should group non-call edges by kind"
        );

        let (status, subgraph) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/graph/subgraph?node_id=n-route&limit_nodes=3&limit_edges=2",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(subgraph["seed_id"], "n-route");
        assert_eq!(subgraph["capped"]["nodes"], true);
        let nodes = subgraph["nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("expected subgraph nodes array"));
        let edges = subgraph["edges"]
            .as_array()
            .unwrap_or_else(|| panic!("expected subgraph edges array"));
        assert_eq!(nodes.len(), 3);
        assert_eq!(edges.len(), 2);

        // Tighter edge limit: 2 edges exist among the visible nodes, cap at 1.
        let (status, capped) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/graph/subgraph?node_id=n-route&limit_nodes=3&limit_edges=1",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(capped["capped"]["edges"], true);
        assert_eq!(
            capped["edges"].as_array().map_or(0, |rows| rows.len()),
            1,
            "edge list should be truncated to the cap"
        );
        assert!(
            nodes
                .iter()
                .any(|node| node["id"] == "n-route" && node["degree"] == 3),
            "subgraph nodes should carry total degree counts (n-route has 3 edges)"
        );
    });
}

#[test]
fn graph_api_finds_shortest_path_and_analytics() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture().await;
        let agent = http_agent();

        // dashboard -> route_graph -> render_graph is the only path.
        let (status, path) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/graph/path?from=n-dashboard&to=n-render",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(path["found"], true);
        assert_eq!(
            path["path"],
            serde_json::json!(["n-dashboard", "n-route", "n-render"])
        );
        let path_edges = path["edges"]
            .as_array()
            .unwrap_or_else(|| panic!("expected path edges array"));
        assert_eq!(path_edges.len(), 2);
        assert!(
            path["nodes"].as_array().is_some_and(|rows| rows.len() == 3),
            "path payload should hydrate full node rows"
        );

        // No path between disconnected nodes within depth.
        let (status, no_path) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/graph/path?from=n-render&to=n-missing",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(no_path["found"], false);

        // Landing analytics: most-connected symbols + largest files.
        let (status, overview) = get_json(
            &agent,
            &format!("{}/api/plugins/graph/overview", fixture.base_url),
        );
        assert_eq!(status, 200);
        let top = overview["top_connected"]
            .as_array()
            .unwrap_or_else(|| panic!("expected top_connected array"));
        assert!(
            top.iter()
                .any(|row| row["id"] == "n-route" && row["degree"] == 3),
            "top_connected should rank n-route with degree 3"
        );
        let largest = overview["largest_files"]
            .as_array()
            .unwrap_or_else(|| panic!("expected largest_files array"));
        assert!(
            largest
                .iter()
                .any(|row| row["path"] == "src/dashboard/mod.rs"),
            "largest_files should include the seeded rust file"
        );
    });
}
