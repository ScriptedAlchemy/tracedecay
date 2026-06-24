//! First-touch creation of the profile store via `tracedecay tool`.
//!
//! The generated Hermes plugin anchors fact/memory/transcript tools at the
//! Hermes home with `--project <home>`. A fresh profile has no `.tracedecay`
//! there yet, so those tools must create the store on first touch instead of
//! failing with "run tracedecay init". Code-graph tools keep the strict
//! no-first-touch behaviour, as does any store tool invoked without an
//! explicit `--project`.

mod common;

use std::path::Path;

use common::{canonical_existing_path, tracedecay_command_with_home};
use tempfile::TempDir;

fn canonical_temp_path(path: &Path) -> std::path::PathBuf {
    canonical_existing_path(path)
}

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
            "fact_store",
            "--json",
            "--args",
            r#"{"action":"add","content":"first touch creates the store","fact_type":"decision"}"#,
        ],
    );
    assert!(
        output.status.success(),
        "fact_store should bootstrap the profile store\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let graph_db_path =
        tracedecay::storage::resolve_layout(&profile, &home_path.join(".tracedecay"))
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
            "fact_store",
            "--json",
            "--args",
            r#"{"action":"search","query":"first touch creates"}"#,
        ],
    );
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("first touch creates the store"),
        "search should find the fact stored at first touch, got:\n{stdout}"
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

#[test]
fn code_graph_tools_do_not_first_touch_project_store() {
    let target = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let target_path = canonical_temp_path(target.path());
    let cwd_path = canonical_temp_path(cwd.path());
    let home_path = canonical_temp_path(home.path());
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
