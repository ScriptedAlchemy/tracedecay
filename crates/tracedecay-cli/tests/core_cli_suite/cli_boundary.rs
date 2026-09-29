use std::path::Path;
use std::process::Command;

use tracedecay_mcp::get_tool_definitions_with_budget;

use crate::common::apply_isolated_profile_env;

fn sandboxed_command(program: impl AsRef<std::ffi::OsStr>, home: &Path) -> Command {
    let mut command = Command::new(program);
    apply_isolated_profile_env(&mut command, home, &home.join(".tracedecay"));
    command
}

#[cfg(feature = "hotpath")]
fn hotpath_command(home: &Path) -> Command {
    let mut command = sandboxed_command(env!("CARGO_BIN_EXE_tracedecay"), home);
    command
        .env_remove("HOTPATH_OUTPUT_FORMAT")
        .env_remove("HOTPATH_OUTPUT_PATH")
        .env_remove("HOTPATH_FOCUS")
        .env("HOTPATH_METRICS_SERVER_OFF", "true");
    command
}

#[cfg(unix)]
#[test]
fn shipped_binary_stops_quietly_when_a_pipeline_reader_exits() {
    let home = tempfile::tempdir().expect("isolated home");
    let output = sandboxed_command("/bin/sh", home.path())
        .args(["-c", r#""$TRACEDECAY_BIN" tool | head -n 4"#])
        .env("TRACEDECAY_BIN", env!("CARGO_BIN_EXE_tracedecay"))
        // A hotpath-enabled binary binds its metrics port on start; when a
        // sibling test's daemon already holds it, the bind failure lands on
        // stderr and breaks the quiet-pipeline assertion below.
        .env("HOTPATH_METRICS_SERVER_OFF", "true")
        .output()
        .expect("tracedecay tool pipeline should run");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 4);
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Help runs without a project graph, so it describes the tool without a
/// node count or call budget it cannot compute.
#[test]
fn context_help_describes_the_tool_without_a_project_size() {
    let home = tempfile::tempdir().expect("isolated home");
    let output = sandboxed_command(env!("CARGO_BIN_EXE_tracedecay"), home.path())
        .args(["tool", "context", "--help"])
        .env("HOTPATH_METRICS_SERVER_OFF", "true")
        .output()
        .expect("run tool help");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let description = stdout.lines().nth(2).expect("description line");
    let sized = get_tool_definitions_with_budget(6_000, 5)
        .expect("budgeted definitions")
        .into_iter()
        .find(|definition| definition.name == "tracedecay_context")
        .expect("context definition")
        .description;
    assert_eq!(
        sized,
        format!("{description} This project (6000 nodes) allows 5 broad context calls."),
        "{stdout}"
    );
    assert!(!description.contains("nodes"), "{stdout}");
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn production_feature_profile_ignores_hotpath_environment() {
    let temp = tempfile::tempdir().expect("temporary report directory");
    let report = temp.path().join("hotpath.json");
    std::fs::write(&report, b"sentinel").expect("seed report sentinel");

    let output = sandboxed_command(env!("CARGO_BIN_EXE_tracedecay"), temp.path())
        .arg("--help")
        .env("TRACEDECAY_HOTPATH", "1")
        .env("HOTPATH_OUTPUT_FORMAT", "json")
        .env("HOTPATH_OUTPUT_PATH", &report)
        .output()
        .expect("run production feature profile with profiling environment");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read(&report).expect("read report sentinel"),
        b"sentinel"
    );
}

#[cfg(feature = "hotpath")]
#[test]
fn compiled_hotpath_profiles_without_a_runtime_environment_gate() {
    let temp = tempfile::tempdir().expect("temporary report directory");
    let report = temp.path().join("hotpath.json");

    let output = hotpath_command(temp.path())
        .arg("--help")
        .env("HOTPATH_OUTPUT_FORMAT", "json")
        .env("HOTPATH_OUTPUT_PATH", &report)
        .output()
        .expect("run feature-on profiling binary");

    assert!(output.status.success(), "{output:?}");
    let text = std::fs::read_to_string(&report).expect("feature-on binary must write a report");
    let report: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("Hotpath report is not JSON: {error}\n{text}"));
    let measured = report
        .pointer("/functions_timing/data")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("report has no functions_timing data: {report}"))
        .iter()
        .filter_map(|entry| entry.get("name").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    assert!(
        measured
            .iter()
            .any(|name| name.contains("cli.hotpath.install_shutdown_finalizer")),
        "process guard installation must be measured: {measured:?}"
    );
}

#[cfg(feature = "hotpath")]
#[test]
fn native_hook_invalid_hotpath_config_preserves_protocol_and_output_path() {
    let temp = tempfile::tempdir().expect("temporary report directory");
    let report = temp.path().join("hotpath.json");
    std::fs::write(&report, b"sentinel").expect("seed report sentinel");

    let output = hotpath_command(temp.path())
        .arg("hook-pre-tool-use")
        .env("HOTPATH_OUTPUT_FORMAT", "invalid")
        .env("HOTPATH_OUTPUT_PATH", &report)
        .env("HOTPATH_FOCUS", "/[/")
        .output()
        .expect("run native hook with invalid Hotpath configuration");

    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "protocol stdout: {output:?}");
    assert_eq!(
        std::fs::read(&report).expect("read report sentinel"),
        b"sentinel"
    );
}

#[cfg(feature = "hotpath")]
#[test]
fn explicit_none_does_not_truncate_the_output_path() {
    let temp = tempfile::tempdir().expect("temporary report directory");
    let report = temp.path().join("hotpath.json");
    std::fs::write(&report, b"sentinel").expect("seed report sentinel");

    let output = hotpath_command(temp.path())
        .arg("hook-pre-tool-use")
        .env("HOTPATH_OUTPUT_FORMAT", "NoNe")
        .env("HOTPATH_OUTPUT_PATH", &report)
        .output()
        .expect("run native hook with reporting disabled");

    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "protocol stdout: {output:?}");
    assert_eq!(
        std::fs::read(&report).expect("read report sentinel"),
        b"sentinel"
    );
}
