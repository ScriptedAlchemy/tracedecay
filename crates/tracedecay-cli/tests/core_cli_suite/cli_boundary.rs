use std::path::Path;
use std::process::Command;

use tracedecay_mcp::get_tool_definitions_with_budget;

use crate::common::apply_isolated_profile_env;

fn sandboxed_command(program: impl AsRef<std::ffi::OsStr>, home: &Path) -> Command {
    let mut command = Command::new(program);
    apply_isolated_profile_env(&mut command, home, &home.join(".tracedecay"));
    command
}

#[cfg(unix)]
#[test]
fn shipped_binary_stops_quietly_when_a_pipeline_reader_exits() {
    let home = tempfile::tempdir().expect("isolated home");
    let output = sandboxed_command("/bin/sh", home.path())
        .args(["-c", r#""$TRACEDECAY_BIN" tool | head -n 4"#])
        .env("TRACEDECAY_BIN", env!("CARGO_BIN_EXE_tracedecay"))
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
