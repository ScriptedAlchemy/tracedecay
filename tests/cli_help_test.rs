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
fn top_level_subcommands_accept_help() {
    for command in [
        "init",
        "sync",
        "status",
        "tool",
        "lsp",
        "install",
        "reinstall",
        "update-plugin",
        "uninstall",
        "dashboard",
        "serve",
        "daemon",
        "upgrade",
        "update",
        "channel",
        "current-counter",
        "reset-counter",
        "disable-upload-counter",
        "enable-upload-counter",
        "gitignore",
        "doctor",
        "cost",
        "bench",
        "gain",
        "monitor",
        "sessions",
        "projects",
        "branch",
        "memory",
        "automation",
        "migrate",
        "wipe",
        "list",
    ] {
        assert_help_succeeds(&[command, "--help"], "Usage:");
    }
}

#[test]
fn nested_subcommands_accept_help() {
    for args in [
        &["lsp", "servers", "--help"][..],
        &["daemon", "run", "--help"],
        &["daemon", "install-service", "--help"],
        &["sessions", "ingest", "--help"],
        &["sessions", "search", "--help"],
        &["projects", "list", "--help"],
        &["projects", "search", "--help"],
        &["projects", "context", "--help"],
        &["branch", "list", "--help"],
        &["branch", "add", "--help"],
        &["memory", "status", "--help"],
        &["memory", "curate", "--help"],
        &["automation", "config", "--help"],
        &["automation", "config", "get", "--help"],
        &["automation", "run", "--help"],
        &["automation", "run", "memory-curation", "--help"],
        &["automation", "runs", "list", "--help"],
        &["automation", "skills", "list", "--help"],
        &["automation", "facts", "list", "--help"],
        &["migrate", "plan", "--help"],
        &["migrate", "registry-gc", "--help"],
    ] {
        assert_help_succeeds(args, "Usage:");
    }
}

#[test]
fn tool_name_help_still_prints_tool_schema() {
    assert_help_succeeds(&["tool", "search", "--help"], "tracedecay tool search");
}
