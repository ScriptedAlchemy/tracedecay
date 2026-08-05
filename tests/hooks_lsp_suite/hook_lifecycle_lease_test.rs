use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use super::common::{
    apply_tracedecay_home_env, git_program, spawn_tracedecay_daemon, tracedecay_command_with_home,
};

const NO_INPUT_HOOKS: &[&str] = &["hook-pre-tool-use", "hook-prompt-submit", "hook-stop"];
const STDIN_HOOKS: &[&str] = &[
    "hook-claude-session-start",
    "hook-claude-post-compact",
    "hook-claude-post-tool-use",
    "hook-claude-subagent-start",
    "hook-kiro-pre-tool-use",
    "hook-kiro-prompt-submit",
    "hook-kiro-post-tool-use",
    "hook-cursor-subagent-start",
    "hook-cursor-post-tool-use",
    "hook-cursor-before-submit-prompt",
    "hook-cursor-pre-compact",
    "hook-cursor-after-file-edit",
    "hook-cursor-session-start",
    "hook-cursor-session-end",
    "hook-cursor-after-shell",
    "hook-cursor-workspace-open",
    "hook-cursor-stop",
    "hook-codex-session-start",
    "hook-codex-user-prompt-submit",
    "hook-codex-subagent-start",
    "hook-codex-post-tool-use",
    "hook-codex-post-compact",
    "hook-codex-stop",
];

fn hold_external_exclusive_lease(home: &Path) -> File {
    let profile = home.join(".tracedecay");
    std::fs::create_dir_all(&profile).unwrap();
    let mut lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(profile.join("lifecycle.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    writeln!(lock, "external-token\tmigration\t999").unwrap();
    lock.flush().unwrap();
    lock
}

fn run_hook(home: &Path, hook: &str, input: Option<&[u8]>) -> Output {
    run_hook_at(home, home, hook, input)
}

fn run_hook_at(home: &Path, cwd: &Path, hook: &str, input: Option<&[u8]>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    apply_tracedecay_home_env(&mut command, home);
    command
        .arg(hook)
        .current_dir(cwd)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    child.wait_with_output().unwrap()
}

#[test]
fn exclusive_lifecycle_owner_quiesces_every_hook_before_startup_or_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let profile = home.join(".tracedecay");
    std::fs::create_dir_all(&profile).unwrap();
    let config = profile.join("config.toml");
    let config_bytes = b"upload_enabled = false\npending_upload = 41\n";
    std::fs::write(&config, config_bytes).unwrap();
    let _exclusive = hold_external_exclusive_lease(home);

    for hook in NO_INPUT_HOOKS {
        let output = run_hook(home, hook, None);
        assert!(output.status.success(), "{hook}: {output:?}");
        assert!(output.stdout.is_empty(), "{hook} wrote stdout");
        assert!(output.stderr.is_empty(), "{hook} wrote stderr");
    }
    for hook in STDIN_HOOKS {
        let payload = if *hook == "hook-claude-session-start" {
            vec![b' '; 256 * 1024]
        } else {
            b"{}".to_vec()
        };
        let output = run_hook(home, hook, Some(&payload));
        assert!(output.status.success(), "{hook}: {output:?}");
        assert!(output.stdout.is_empty(), "{hook} wrote stdout");
        assert!(output.stderr.is_empty(), "{hook} wrote stderr");
    }

    assert_eq!(std::fs::read(&config).unwrap(), config_bytes);
    assert!(!profile.join("global.db").exists());
    assert!(!profile.join("projects").exists());
}

#[test]
fn normal_lease_path_still_executes_a_direct_claude_stdin_hook() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn hook_fixture() {}\n").unwrap();
    let git = git_program();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "test@tracedecay.dev"][..],
        &["config", "user.name", "TraceDecay Test"][..],
        &["add", "."][..],
        &["commit", "-q", "-m", "fixture"][..],
    ] {
        assert!(
            Command::new(&git)
                .args(args)
                .current_dir(&project)
                .status()
                .unwrap()
                .success()
        );
    }
    let _daemon = spawn_tracedecay_daemon(temp.path());
    assert!(
        tracedecay_command_with_home(temp.path())
            .arg("init")
            .current_dir(&project)
            .status()
            .unwrap()
            .success()
    );
    let event = format!(
        "{{\"hook_event_name\":\"SessionStart\",\"cwd\":{}}}",
        serde_json::to_string(&project.to_string_lossy()).unwrap()
    );

    let output = run_hook_at(
        temp.path(),
        &project,
        "hook-claude-session-start",
        Some(event.as_bytes()),
    );

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert!(serde_json::from_slice::<serde_json::Value>(&output.stdout).is_ok());
}

#[test]
fn lifecycle_path_error_silently_drains_and_quiesces_the_hook() {
    let temp = tempfile::tempdir().unwrap();
    let profile_file = temp.path().join(".tracedecay");
    std::fs::write(&profile_file, b"not a profile directory").unwrap();
    let payload = vec![b' '; 256 * 1024];

    let output = run_hook(temp.path(), "hook-claude-session-start", Some(&payload));

    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(
        std::fs::read(profile_file).unwrap(),
        b"not a profile directory"
    );
}
