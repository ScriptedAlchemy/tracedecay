//! First-touch creation of the profile store via `tracedecay tool`.
//!
//! The generated Hermes plugin anchors fact/memory/transcript tools at the
//! Hermes home with `--project <home>`. A fresh profile has no `.tracedecay`
//! there yet, so those tools must create the store on first touch instead of
//! failing with "run tracedecay init". Code-graph tools keep the strict
//! no-first-touch behaviour, as does any store tool invoked without an
//! explicit `--project`.

use std::path::Path;

use crate::common;
use crate::common::{canonical_existing_path as canonical_temp_path, tracedecay_command_with_home};
use tempfile::TempDir;

fn run_tool(cwd: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    tracedecay_command_with_home(home)
        .current_dir(cwd)
        .arg("tool")
        .args(args)
        .output()
        .expect("failed to spawn tracedecay")
}

#[cfg(unix)]
#[test]
fn fact_store_creates_profile_store_on_first_touch() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let home_path = canonical_temp_path(home.path());
    let cwd_path = canonical_temp_path(cwd.path());
    let profile = home_path.join(".hermes");
    std::fs::create_dir_all(&profile).unwrap();
    let _daemon = common::spawn_tracedecay_daemon(&home_path);

    let profile_arg = profile.to_string_lossy().to_string();
    let output = run_tool(
        &cwd_path,
        &home_path,
        &[
            "--project",
            &profile_arg,
            "fact_store_add",
            "--json",
            "--args",
            r#"{"content":"first touch creates the store","category":"decision"}"#,
        ],
    );
    assert!(
        output.status.success(),
        "fact_store_add through a live daemon should bootstrap the profile store\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let graph_db_path =
        tracedecay_runtime_core::storage::resolve_layout(&profile, &home_path.join(".tracedecay"))
            .unwrap()
            .graph_db_path;
    assert!(
        graph_db_path.is_file(),
        "first touch should have created the resolved profile graph DB at {}",
        graph_db_path.display()
    );

    // The store persists: a follow-up search finds the fact.
    let output = run_tool(
        &cwd_path,
        &home_path,
        &[
            "--project",
            &profile_arg,
            "fact_store_search",
            "--json",
            "--args",
            r#"{"query":"first touch creates"}"#,
        ],
    );
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Status: `success`") && stdout.contains("returned=1"),
        "fact_store_search should return the first-touch fact as a one-hit evidence page, got:\n{stdout}"
    );
}

#[test]
fn store_tools_without_explicit_project_still_require_init() {
    let cwd = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let cwd_path = canonical_temp_path(cwd.path());
    let home_path = canonical_temp_path(home.path());
    let output = run_tool(
        &cwd_path,
        &home_path,
        &["fact_store", "--args", r#"{"action":"list"}"#],
    );
    assert!(
        !output.status.success(),
        "without --project an uninitialised cwd must keep the init guidance"
    );
    assert!(
        !cwd_path.join(".tracedecay").exists(),
        "no store may be silently created in the working directory"
    );
}

/// Registry reads answer from the profile's project registry and need no
/// initialised project: from a plain directory, and with an explicit
/// uninitialised `--project`, `project_list` returns the same typed empty
/// registry `tracedecay projects list` reports, and creates no store.
#[cfg(unix)]
#[test]
fn project_list_answers_from_the_registry_without_an_initialised_project() {
    let target = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let target_path = canonical_temp_path(target.path());
    let cwd_path = canonical_temp_path(cwd.path());
    let home_path = canonical_temp_path(home.path());
    let _daemon = common::spawn_tracedecay_daemon(&home_path);
    let target_arg = target_path.to_string_lossy().to_string();

    let bare = run_tool(&cwd_path, &home_path, &["project_list", "--json"]);
    let explicit = run_tool(
        &cwd_path,
        &home_path,
        &["--project", &target_arg, "project_list", "--json"],
    );
    for (label, output) in [
        ("bare", bare),
        ("explicit uninitialised --project", explicit),
    ] {
        assert!(
            output.status.success(),
            "{label}: project_list must answer without an initialised project\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let result: serde_json::Value =
            serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
                panic!("{label}: --json must print one JSON object ({error}):\n{stdout}")
            });
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{label}: missing text content:\n{result}"));
        let payload: serde_json::Value = serde_json::from_str(text)
            .unwrap_or_else(|error| panic!("{label}: text is not JSON ({error}):\n{text}"));
        assert_eq!(payload["status"], "ok", "{label}: {payload}");
        assert_eq!(
            payload["projects"],
            serde_json::json!([]),
            "{label}: {payload}"
        );
        assert_eq!(
            payload["summary"]["project_count"],
            serde_json::json!(0),
            "{label}: {payload}"
        );
        assert_ne!(
            result["isError"],
            serde_json::json!(true),
            "{label}: an empty registry is not a failure: {result}"
        );
    }
    assert!(
        !cwd_path.join(".tracedecay").exists() && !target_path.join(".tracedecay").exists(),
        "registry reads must never create a project store"
    );

    // `tracedecay projects list` reads the same registry; the two surfaces
    // must agree on the empty profile.
    let projects = tracedecay_command_with_home(&home_path)
        .current_dir(&cwd_path)
        .args(["projects", "list", "--json"])
        .output()
        .expect("failed to spawn tracedecay");
    assert!(
        projects.status.success(),
        "projects list failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&projects.stdout),
        String::from_utf8_lossy(&projects.stderr)
    );
    let projects: serde_json::Value =
        serde_json::from_slice(&projects.stdout).expect("projects list --json payload");
    assert_eq!(projects["status"], "ok");
    assert_eq!(projects["projects"], serde_json::json!([]));
}

#[test]
fn code_graph_tools_do_not_first_touch_project_store() {
    let target = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let target_path = canonical_temp_path(target.path());
    let cwd_path = canonical_temp_path(cwd.path());
    let home_path = canonical_temp_path(home.path());
    let _daemon = common::spawn_tracedecay_daemon(&home_path);
    let target_arg = target_path.to_string_lossy().to_string();
    let output = run_tool(
        &cwd_path,
        &home_path,
        &["--project", &target_arg, "status", "--json"],
    );
    assert!(
        !output.status.success(),
        "code-graph tools must not first-touch create project stores"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("run 'tracedecay init' first")
            || stderr.contains("run `tracedecay init` first"),
        "expected init guidance, got:\n{stderr}"
    );
    assert!(!target_path.join(".tracedecay").exists());
}
