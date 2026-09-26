//! Paging a relation with `tracedecay tool`: every page is a separate CLI
//! process, so a `next_cursor` must be redeemable by a later invocation, and
//! a cursor presented where it cannot be served must say why instead of
//! answering a retryable `unavailable`.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::common::{
    canonical_existing_path, git_program, initialize_tracedecay_cli_project, stop_managed_daemon,
    tracedecay_command_with_home,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const LEAF_COUNT: usize = 25;
/// Indexing after `init` is asynchronous; this only bounds a hang.
const INDEX_READY_TIMEOUT: Duration = Duration::from_secs(90);

fn git(project: &Path, args: &[&str]) {
    let output = std::process::Command::new(git_program())
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?} should run: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn committed_git_project(project: &Path, source: &str) {
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), source).unwrap();
    git(project, &["init", "--initial-branch=master"]);
    git(project, &["config", "user.email", "cursor@example.com"]);
    git(project, &["config", "user.name", "Cursor Test"]);
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "initial"]);
}

/// `hub` calls `leaf_01` .. `leaf_25`: three pages at the CLI page size.
fn hub_source() -> String {
    let calls: String = (1..=LEAF_COUNT)
        .map(|leaf| format!("    leaf_{leaf:02}();\n"))
        .collect();
    let leaves: String = (1..=LEAF_COUNT)
        .map(|leaf| format!("pub fn leaf_{leaf:02}() {{}}\n"))
        .collect();
    format!("pub fn hub() {{\n{calls}}}\n{leaves}")
}

struct ToolRun {
    success: bool,
    stdout: String,
    stderr: String,
}

fn run_tool(home: &Path, cwd: &Path, name: &str, args: &Value) -> ToolRun {
    let output = tracedecay_command_with_home(home)
        .current_dir(cwd)
        .args(["tool", name, "--json", "--args", &args.to_string()])
        .output()
        .expect("tracedecay tool should run");
    ToolRun {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn body_of(name: &str, run: &ToolRun) -> Value {
    let printed: Value = serde_json::from_str(&run.stdout).unwrap_or_else(|error| {
        panic!(
            "{name} printed non-JSON ({error}):\n{}\nstderr:\n{}",
            run.stdout, run.stderr
        )
    });
    match printed["content"][0]["text"].as_str() {
        Some(text) => serde_json::from_str(text).expect("tool text JSON"),
        None => printed,
    }
}

/// One `tracedecay tool <name> --json` process from `cwd`; returns whether
/// the process succeeded and the tool's JSON body.
fn tool(home: &Path, cwd: &Path, name: &str, args: &Value) -> (bool, Value) {
    let run = run_tool(home, cwd, name, args);
    (run.success, body_of(name, &run))
}

fn hub_node_id(home: &Path, project: &Path) -> String {
    let started = Instant::now();
    loop {
        let run = run_tool(
            home,
            project,
            "find_exact_symbol",
            &json!({"name": "hub", "format": "json"}),
        );
        // Until the first graph publishes, the lookup refuses with an empty stdout.
        if run.success
            && let Some(id) = body_of("find_exact_symbol", &run)["matches"][0]["id"].as_str()
        {
            return id.to_owned();
        }
        assert!(
            started.elapsed() < INDEX_READY_TIMEOUT,
            "hub never indexed:\n{}\n{}",
            run.stdout,
            run.stderr
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn callees_args(node_id: &str, cursor: Option<&str>) -> Value {
    let mut meta = json!({"projection": "evidence", "order": "source_position"});
    if let Some(cursor) = cursor {
        meta["cursor"] = json!(cursor);
    }
    json!({"node_id": node_id, "maximum_depth": 1, "meta": meta})
}

fn page_names(body: &Value) -> Vec<String> {
    body["outcome"]["value"]["payload"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("callees page has no items: {body}"))
        .iter()
        .map(|item| item["symbol"]["name"].as_str().unwrap().to_owned())
        .collect()
}

fn next_cursor(body: &Value) -> Option<String> {
    body["outcome"]["value"]["payload"]["next_cursor"]
        .as_str()
        .map(str::to_owned)
}

#[test]
fn callees_cursors_page_to_the_end_across_tool_processes() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home = canonical_existing_path(home.path());
    let project = canonical_existing_path(project.path());
    committed_git_project(&project, &hub_source());
    initialize_tracedecay_cli_project(&home, &project);
    let hub = hub_node_id(&home, &project);

    let mut page_sizes = Vec::new();
    let mut names = BTreeSet::new();
    let mut cursor = None;
    loop {
        let (ok, body) = tool(
            &home,
            &project,
            "tracedecay_callees",
            &callees_args(&hub, cursor.as_deref()),
        );
        assert!(ok, "page {} failed: {body}", page_sizes.len() + 1);
        assert_eq!(body["outcome"]["value"]["payload"]["total"], 25, "{body}");
        let page = page_names(&body);
        page_sizes.push(page.len());
        names.extend(page);
        cursor = next_cursor(&body);
        if cursor.is_none() {
            break;
        }
    }

    assert_eq!(page_sizes, [10, 10, 5]);
    let expected: BTreeSet<String> = (1..=LEAF_COUNT)
        .map(|leaf| format!("leaf_{leaf:02}"))
        .collect();
    assert_eq!(names, expected);
}

#[test]
fn a_cursor_presented_where_it_cannot_be_served_is_typed() {
    let home = TempDir::new().unwrap();
    let other_home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let other_project = TempDir::new().unwrap();
    let unenrolled = TempDir::new().unwrap();
    let no_repository = TempDir::new().unwrap();
    let home = canonical_existing_path(home.path());
    let other_home = canonical_existing_path(other_home.path());
    let project = canonical_existing_path(project.path());
    let other_project = canonical_existing_path(other_project.path());
    let unenrolled = canonical_existing_path(unenrolled.path());
    let no_repository = canonical_existing_path(no_repository.path());
    committed_git_project(&project, &hub_source());
    committed_git_project(
        &other_project,
        "pub fn hub() { leaf(); }\npub fn leaf() {}\n",
    );
    committed_git_project(&unenrolled, "pub fn unenrolled() {}\n");
    // Enrolled by another profile: the checkout carries a TraceDecay identity
    // marker, so this profile's CLI routes to it, but this profile's daemon
    // never enrolled it.
    initialize_tracedecay_cli_project(&other_home, &unenrolled);
    stop_managed_daemon(&other_home);
    initialize_tracedecay_cli_project(&home, &project);
    initialize_tracedecay_cli_project(&home, &other_project);
    let hub = hub_node_id(&home, &project);
    hub_node_id(&home, &other_project);
    let (ok, first) = tool(
        &home,
        &project,
        "tracedecay_callees",
        &callees_args(&hub, None),
    );
    assert!(ok, "{first}");
    let cursor = next_cursor(&first).expect("first page continues");

    // Another enrolled project cannot serve this project's cursor.
    let (_, foreign) = tool(
        &home,
        &other_project,
        "tracedecay_callees",
        &callees_args(&hub, Some(&cursor)),
    );
    let value = &foreign["outcome"]["value"];
    assert_eq!(
        value["omissions"],
        json!([{"domain": "symbol", "count": 0, "reason": "cursor_foreign"}]),
        "{foreign}"
    );
    assert_eq!(value["execution"]["termination"], "failed", "{foreign}");

    // A checkout this profile never enrolled has no project to redeem in.
    let (ok, outside) = tool(
        &home,
        &unenrolled,
        "tracedecay_callees",
        &callees_args(&hub, Some(&cursor)),
    );
    assert!(!ok, "{outside}");
    assert_eq!(outside["problem"]["kind"], "invalid_request", "{outside}");
    assert_eq!(
        outside["problem"]["code"], "project_not_enrolled",
        "{outside}"
    );
    assert_eq!(outside["problem"]["retryable"], false, "{outside}");
    assert_eq!(
        outside["problem"]["legal_actions"],
        json!(["correct_request"]),
        "{outside}"
    );

    // A directory outside any repository names no project at all.
    let (ok, projectless) = tool(
        &home,
        &no_repository,
        "tracedecay_callees",
        &callees_args(&hub, Some(&cursor)),
    );
    assert!(!ok, "{projectless}");
    assert_eq!(
        projectless["problem"]["code"], "project_required",
        "{projectless}"
    );
    assert_eq!(projectless["problem"]["retryable"], false, "{projectless}");

    // The same cursor still pages where it was issued.
    let (ok, second) = tool(
        &home,
        &project,
        "tracedecay_callees",
        &callees_args(&hub, Some(&cursor)),
    );
    assert!(ok, "{second}");
    assert_eq!(page_names(&second).len(), 10, "{second}");
    assert!(
        page_names(&first)
            .iter()
            .all(|name| !page_names(&second).contains(name)),
        "page two repeated page one: {first} / {second}"
    );
}
