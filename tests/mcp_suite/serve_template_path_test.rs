//! `serve --path` tolerance for literal unexpanded `${...}` host template
//! variables (e.g. Cursor headless agent-session MCP scopes spawning
//! `serve --path ${workspaceFolder}` verbatim from the user home). See
//! `cursor-plugin/README.md` for the full rationale.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use serde_json::json;
use tempfile::TempDir;

use crate::common::canonical_existing_path;
use crate::serve_harness::{
    canonical_path_string, degraded_tool_error_text, init_project_under, init_project_with_file,
    json_rpc_response, register_global_project, run_serve_runtime, runtime_project_root,
};

const UNEXPANDED_TEMPLATE_WARNING: &str = "unexpanded template variable";
const PROJECT_CHOICE_LOG: &str = "tracedecay serve: using project";

fn run_serve_runtime_with_path_arg(
    home: &Path,
    cwd: &Path,
    path_arg: &str,
) -> std::process::Output {
    run_serve_runtime(home, cwd, Some(OsStr::new(path_arg)), json!({}))
}

/// Exact reproduction of the Cursor headless agent-session spawn: the host
/// launches `tracedecay serve --path ${workspaceFolder}` with the LITERAL
/// (unexpanded) template variable and cwd set to the user home, not the
/// workspace. `serve` must warn, discard the bogus path, and complete the MCP
/// initialize handshake via discovery instead of exiting with a config error
/// (Cursor never retries a failed MCP scope, so an early exit permanently
/// breaks the connection).
#[tokio::test]
async fn literal_workspace_folder_path_from_home_cwd_serves_via_discovery() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn headless_scope_marker() {}\n").await;
    register_global_project(home.path(), project.path()).await;

    let home_cwd = canonical_existing_path(home.path());
    let output = run_serve_runtime_with_path_arg(home.path(), &home_cwd, "${workspaceFolder}");

    assert!(
        output.status.success(),
        "serve must tolerate a literal ${{workspaceFolder}} --path instead of exiting\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"protocolVersion\":\"2024-11-05\""),
        "serve should answer the MCP initialize handshake\nstdout:\n{stdout}"
    );
    assert_eq!(
        canonical_path_string(Path::new(&runtime_project_root(&output.stdout, 2))),
        canonical_path_string(project.path()),
        "serve should resolve the registered project via discovery"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(UNEXPANDED_TEMPLATE_WARNING)
            && stderr.contains("${workspaceFolder}")
            && stderr.contains("project discovery"),
        "serve should warn that the host passed an unexpanded template variable\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains(PROJECT_CHOICE_LOG),
        "serve should log which project the fallback picked and why\nstderr:\n{stderr}"
    );
}

/// Other unexpanded `${...}` forms (different variable names, default-value
/// syntax) must get the same tolerance as `${workspaceFolder}`.
#[tokio::test]
async fn literal_template_variant_paths_fall_back_to_discovery() {
    for path_arg in ["${workspaceRoot}", "${workspaceFolder:-/tmp/never-used}"] {
        let home = TempDir::new().unwrap();
        let project =
            init_project_with_file(home.path(), "pub fn template_variant_marker() {}\n").await;
        register_global_project(home.path(), project.path()).await;
        let cwd = TempDir::new().unwrap();

        let output = run_serve_runtime_with_path_arg(home.path(), cwd.path(), path_arg);

        assert!(
            output.status.success(),
            "serve must tolerate the literal template path {path_arg}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            canonical_path_string(Path::new(&runtime_project_root(&output.stdout, 2))),
            canonical_path_string(project.path()),
            "serve should resolve the registered project via discovery for {path_arg}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(UNEXPANDED_TEMPLATE_WARNING) && stderr.contains(path_arg),
            "warning should name the unexpanded variable for {path_arg}\nstderr:\n{stderr}"
        );
    }
}

/// A real directory whose name merely contains `$` (no `${...}` syntax) is a
/// legitimate explicit path and must NOT be diverted through discovery.
#[tokio::test]
async fn explicit_path_with_dollar_sign_directory_is_not_treated_as_template() {
    let home = TempDir::new().unwrap();
    let parent = TempDir::new().unwrap();
    let dollar_project = init_project_under(
        home.path(),
        parent.path(),
        "pri$ce-project",
        "pub fn dollar_dir_marker() {}\n",
    )
    .await;
    // A registered decoy proves the explicit path stays authoritative: if the
    // `$` path were misread as a template, discovery would serve the decoy.
    let decoy = init_project_with_file(home.path(), "pub fn decoy_marker() {}\n").await;
    register_global_project(home.path(), decoy.path()).await;
    let cwd = TempDir::new().unwrap();

    let output =
        run_serve_runtime_with_path_arg(home.path(), cwd.path(), dollar_project.to_str().unwrap());

    assert!(
        output.status.success(),
        "serve should accept an explicit path containing '$'\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        canonical_path_string(Path::new(&runtime_project_root(&output.stdout, 2))),
        canonical_path_string(&dollar_project),
        "the explicit '$' directory must be served, not a discovery fallback"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains(UNEXPANDED_TEMPLATE_WARNING),
        "a real '$' directory must not trigger the template warning\nstderr:\n{stderr}"
    );
}

/// A genuinely wrong explicit path (no template syntax) must keep failing
/// loudly — the template tolerance must not swallow real misconfiguration.
/// It does NOT exit though (a dead process permanently kills the MCP scope):
/// it serves degraded, reporting the missing path from every tool call and
/// never falling back to discovery.
#[tokio::test]
async fn explicit_nonexistent_path_reports_degraded_error_instead_of_discovery() {
    let home = TempDir::new().unwrap();
    let active = init_project_with_file(home.path(), "pub fn active_project_marker() {}\n").await;
    register_global_project(home.path(), active.path()).await;
    let scratch = TempDir::new().unwrap();
    let missing = scratch.path().join("does-not-exist");
    let cwd = TempDir::new().unwrap();

    let output = run_serve_runtime(
        home.path(),
        cwd.path(),
        Some(missing.as_os_str()),
        json!({}),
    );

    assert!(
        output.status.success(),
        "a nonexistent explicit --path serves degraded instead of exiting\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = degraded_tool_error_text(&json_rpc_response(&output.stdout, 2));
    assert!(
        text.contains(&missing.display().to_string()),
        "the degraded error should name the missing explicit path\n{text}"
    );
    assert!(
        !text.contains(&active.path().display().to_string()),
        "an explicit path must never fall back to the registered discovery project\n{text}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&missing.display().to_string()),
        "error should name the missing explicit path\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains(UNEXPANDED_TEMPLATE_WARNING),
        "a plain nonexistent path must not be mistaken for a template\nstderr:\n{stderr}"
    );
}

/// Pins the template decision: no-path discovery from a home-directory cwd
/// cannot pick between multiple same-depth registered projects, which is why
/// the Cursor plugin template keeps `--path ${workspaceFolder}` (normal
/// windows expand it) and relies on the literal-template fallback only when
/// the host fails to expand. When that fallback cannot find a unique project,
/// the actionable ambiguity error must surface rather than an arbitrary
/// project.
#[tokio::test]
async fn literal_workspace_folder_with_multiple_projects_reports_ambiguity() {
    let home = TempDir::new().unwrap();
    let home_cwd = canonical_existing_path(home.path());
    let projects_dir = home_cwd.join("projects");
    fs::create_dir_all(&projects_dir).unwrap();
    let alpha = init_project_under(
        home.path(),
        &projects_dir,
        "alpha",
        "pub fn alpha_marker() {}\n",
    )
    .await;
    let beta = init_project_under(
        home.path(),
        &projects_dir,
        "beta",
        "pub fn beta_marker() {}\n",
    )
    .await;
    register_global_project(home.path(), &alpha).await;
    register_global_project(home.path(), &beta).await;

    let output = run_serve_runtime_with_path_arg(home.path(), &home_cwd, "${workspaceFolder}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ambiguity serves degraded (Cursor never retries a dead scope) instead of exiting\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        stderr
    );
    let text = degraded_tool_error_text(&json_rpc_response(&output.stdout, 2));
    assert!(
        text.contains("Multiple tracedecay projects found"),
        "ambiguous discovery must not silently pick a project\n{text}"
    );
    assert!(
        stderr.contains(UNEXPANDED_TEMPLATE_WARNING),
        "the template warning should still be emitted before the ambiguity error\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("Multiple tracedecay projects found"),
        "stderr should surface the actionable ambiguity error\nstderr:\n{stderr}"
    );
}

/// The wrong-project guard: with registered projects at DIFFERENT depths
/// (`~/proj-a` and `~/work/proj-b`), genuine no-path serve may use the cwd
/// depth heuristic (the user ran it from that directory), but the
/// discarded-template fallback must not — the host's spawn directory says
/// nothing about the intended workspace, so silently serving the shallower
/// project would attach the wrong index to the session. It must require a
/// unique registry match and surface the ambiguity error otherwise.
#[tokio::test]
async fn literal_template_with_different_depth_projects_is_stricter_than_no_path() {
    let home = TempDir::new().unwrap();
    let home_cwd = canonical_existing_path(home.path());
    let shallow = init_project_under(
        home.path(),
        &home_cwd,
        "proj-a",
        "pub fn shallow_marker() {}\n",
    )
    .await;
    let work_dir = home_cwd.join("work");
    fs::create_dir_all(&work_dir).unwrap();
    let deep = init_project_under(
        home.path(),
        &work_dir,
        "proj-b",
        "pub fn deep_marker() {}\n",
    )
    .await;
    register_global_project(home.path(), &shallow).await;
    register_global_project(home.path(), &deep).await;

    // Genuine no-path serve resolves via the cwd depth heuristic (shallowest
    // registered descendant of cwd wins) — unchanged behavior.
    let no_path = run_serve_runtime(home.path(), &home_cwd, None, json!({}));
    assert!(
        no_path.status.success(),
        "genuine no-path serve should still resolve via the cwd heuristic\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&no_path.stdout),
        String::from_utf8_lossy(&no_path.stderr)
    );
    assert_eq!(
        canonical_path_string(Path::new(&runtime_project_root(&no_path.stdout, 2))),
        canonical_path_string(&shallow),
        "no-path serve should pick the shallowest registered descendant of cwd"
    );

    // The discarded-template fallback must be stricter: same registry, same
    // cwd, but no unique match — so it reports the ambiguity (from degraded
    // mode) instead of guessing.
    let templated = run_serve_runtime_with_path_arg(home.path(), &home_cwd, "${workspaceFolder}");
    let stderr = String::from_utf8_lossy(&templated.stderr);
    assert!(
        templated.status.success(),
        "the template fallback serves degraded instead of exiting\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&templated.stdout),
        stderr
    );
    let text = degraded_tool_error_text(&json_rpc_response(&templated.stdout, 2));
    assert!(
        text.contains("Multiple tracedecay projects found"),
        "the template fallback must not silently depth-rank projects\n{text}"
    );
    assert!(
        stderr.contains("Multiple tracedecay projects found"),
        "stderr should surface the actionable ambiguity error\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains(PROJECT_CHOICE_LOG),
        "no project-choice log should be emitted when nothing was picked\nstderr:\n{stderr}"
    );
}
