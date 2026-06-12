//! First-touch creation of the profile store via `tokensave tool`.
//!
//! The generated Hermes plugin anchors fact/memory/transcript tools at the
//! Hermes home with `--project <home>`. A fresh profile has no `.tokensave`
//! there yet, so those tools must create the store on first touch instead of
//! failing with "run tokensave init". Code-graph tools keep the strict
//! behaviour, as does any store tool invoked without an explicit `--project`.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn run_tool(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tokensave"))
        .current_dir(cwd)
        .arg("tool")
        .args(args)
        .output()
        .expect("failed to spawn tokensave")
}

#[test]
fn fact_store_creates_profile_store_on_first_touch() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let profile = home.path().join(".hermes");
    std::fs::create_dir_all(&profile).unwrap();

    let profile_arg = profile.to_string_lossy().to_string();
    let output = run_tool(
        cwd.path(),
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
    assert!(
        profile.join(".tokensave").join("tokensave.db").is_file(),
        "first touch should have created .tokensave/tokensave.db under the profile home"
    );

    // The store persists: a follow-up search finds the fact.
    let output = run_tool(
        cwd.path(),
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
    let output = run_tool(
        cwd.path(),
        &["fact_store", "--args", r#"{"action":"list"}"#],
    );
    assert!(
        !output.status.success(),
        "without --project an uninitialised cwd must keep the init guidance"
    );
    assert!(
        !cwd.path().join(".tokensave").exists(),
        "no store may be silently created in the working directory"
    );
}

#[test]
fn code_graph_tools_keep_strict_init_requirement() {
    let target = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let target_arg = target.path().to_string_lossy().to_string();
    let output = run_tool(cwd.path(), &["--project", &target_arg, "status", "--json"]);
    assert!(
        !output.status.success(),
        "code-graph tools must not bootstrap stores on first touch"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no TokenSave index found"),
        "expected init guidance, got:\n{stderr}"
    );
    assert!(!target.path().join(".tokensave").exists());
}
