use std::process::Command;

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
