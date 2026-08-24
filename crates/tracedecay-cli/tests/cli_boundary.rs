use std::process::Command;

#[cfg(feature = "hotpath")]
const TRACEDECAY_HOTPATH_ENV: &str = "TRACEDECAY_HOTPATH";

#[cfg(feature = "hotpath")]
fn hotpath_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    command
        .env_remove(TRACEDECAY_HOTPATH_ENV)
        .env_remove("HOTPATH_OUTPUT_FORMAT")
        .env_remove("HOTPATH_OUTPUT_PATH")
        .env_remove("HOTPATH_FOCUS")
        .env("HOTPATH_METRICS_SERVER_OFF", "true");
    command
}

#[test]
fn shipped_binary_exposes_existing_cli_surface() {
    let output = Command::new(env!("CARGO_BIN_EXE_tracedecay"))
        .arg("--help")
        .output()
        .expect("the workspace should build the tracedecay binary");

    assert!(
        output.status.success(),
        "`tracedecay --help` exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: tracedecay"), "{stdout}");
    assert!(stdout.contains("daemon"), "{stdout}");
    assert!(stdout.contains("tool"), "{stdout}");
}

#[cfg(feature = "hotpath")]
#[test]
fn compiled_hotpath_is_dormant_without_runtime_activation() {
    let temp = tempfile::tempdir().expect("temporary report directory");
    let report = temp.path().join("hotpath.json");
    std::fs::write(&report, b"sentinel").expect("seed report sentinel");

    let output = hotpath_command()
        .arg("--help")
        .env("HOTPATH_OUTPUT_FORMAT", "json")
        .env("HOTPATH_OUTPUT_PATH", &report)
        .output()
        .expect("run feature-on binary without runtime activation");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read(&report).expect("read report sentinel"),
        b"sentinel"
    );
}

#[cfg(feature = "hotpath")]
#[test]
fn runtime_activation_writes_a_hotpath_report() {
    let temp = tempfile::tempdir().expect("temporary report directory");
    let report = temp.path().join("hotpath.json");

    let output = hotpath_command()
        .arg("--help")
        .env(TRACEDECAY_HOTPATH_ENV, "1")
        .env("HOTPATH_OUTPUT_FORMAT", "json")
        .env("HOTPATH_OUTPUT_PATH", &report)
        .output()
        .expect("run runtime-activated feature-on binary");

    assert!(output.status.success(), "{output:?}");
    let bytes = std::fs::read(&report).expect("runtime activation must write a report");
    assert!(!bytes.is_empty(), "Hotpath report must not be empty");
}

#[cfg(feature = "hotpath")]
#[test]
fn native_hook_invalid_hotpath_config_preserves_protocol_and_output_path() {
    let temp = tempfile::tempdir().expect("temporary report directory");
    let report = temp.path().join("hotpath.json");
    std::fs::write(&report, b"sentinel").expect("seed report sentinel");

    let output = hotpath_command()
        .arg("hook-pre-tool-use")
        .env(TRACEDECAY_HOTPATH_ENV, "true")
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

    let output = hotpath_command()
        .arg("hook-pre-tool-use")
        .env(TRACEDECAY_HOTPATH_ENV, "1")
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
