use std::process::Command;

fn tracedecay_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tracedecay")
}

fn assert_help_succeeds(args: &[&str], expected: &str) {
    let output = Command::new(tracedecay_bin())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run tracedecay {args:?}: {e}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "tracedecay {args:?} should exit successfully\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains(expected) || stderr.contains(expected),
        "tracedecay {args:?} should print help containing {expected:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn storage_subcommands_use_contextual_nouns_without_legacy_aliases() {
    for args in [
        &["storage", "report", "--help"][..],
        &["storage", "backup", "--help"],
        &["storage", "rehearse-backup", "--help"],
    ] {
        assert_help_succeeds(args, "Usage:");
    }

    for args in [
        &["storage", "storage-report", "--help"][..],
        &["storage", "backup-profile", "--help"],
        &["storage", "rehearse-profile-backup", "--help"],
    ] {
        let output = Command::new(tracedecay_bin())
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("run tracedecay {args:?}: {e}"));
        assert!(
            !output.status.success(),
            "legacy tracedecay {args:?} should exit nonzero"
        );
    }
}

#[test]
fn legacy_host_cli_aliases_are_rejected() {
    for alias in ["claude-install", "update-plugins", "claude-uninstall"] {
        let output = Command::new(tracedecay_bin())
            .args([alias, "--help"])
            .output()
            .unwrap_or_else(|e| panic!("run tracedecay {alias} --help: {e}"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "removed tracedecay {alias} should exit nonzero\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
