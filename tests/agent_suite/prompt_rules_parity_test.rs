//! CLI-fallback parity for every prompt-rules host.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay::agents::prompt_rules::cli_fallback_paragraph;
use tracedecay::agents::{expected_tool_perms, get_integration, InstallContext};
use tracedecay::config::USER_DATA_DIR_ENV;

use crate::common::{EnvVarGuard, PROCESS_ENV_LOCK};

const STANDARD_MARKER: &str = "## Prefer tracedecay MCP tools";
const CLAUDE_MARKER: &str = "## MANDATORY: No Explore Agents When Tracedecay Is Available";
const KIRO_END_MARKER: &str = "<!-- tracedecay:kiro:end -->";

struct HostCase {
    id: &'static str,
    rules_path: fn(&Path) -> PathBuf,
    marker: &'static str,
    stale_block_tail: &'static str,
}

fn hosts() -> Vec<HostCase> {
    vec![
        HostCase {
            id: "claude",
            rules_path: |home| home.join(".claude/CLAUDE.md"),
            marker: CLAUDE_MARKER,
            stale_block_tail: "",
        },
        HostCase {
            id: "copilot",
            rules_path: |home| home.join(".copilot/copilot-instructions.md"),
            marker: STANDARD_MARKER,
            stale_block_tail: "",
        },
        HostCase {
            id: "gemini",
            rules_path: |home| home.join(".gemini/GEMINI.md"),
            marker: STANDARD_MARKER,
            stale_block_tail: "",
        },
        HostCase {
            id: "opencode",
            rules_path: |home| home.join(".config/opencode/AGENTS.md"),
            marker: STANDARD_MARKER,
            stale_block_tail: "",
        },
        HostCase {
            id: "kimi",
            rules_path: |home| home.join(".kimi/AGENTS.md"),
            marker: STANDARD_MARKER,
            stale_block_tail: "",
        },
        HostCase {
            id: "vibe",
            rules_path: |home| home.join(".vibe/prompts/cli.md"),
            marker: STANDARD_MARKER,
            stale_block_tail: "",
        },
        HostCase {
            id: "kiro",
            rules_path: |home| home.join(".kiro/steering/tracedecay.md"),
            marker: STANDARD_MARKER,
            stale_block_tail: "\n\n<!-- tracedecay:kiro:end -->",
        },
    ]
}

fn install_ctx(home: &Path) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: "/usr/local/bin/tracedecay".to_string(),
        tool_permissions: expected_tool_perms(),
        profile: None,
        project_root: None,
        dashboard: true,
    }
}

/// Pins profile storage into the temp home and clears host-home overrides so
/// installers resolve every path under the throwaway home. Callers must hold
/// [`PROCESS_ENV_LOCK`] while the guards are alive.
fn env_guards(home: &Path) -> Vec<EnvVarGuard> {
    vec![
        EnvVarGuard::set(USER_DATA_DIR_ENV, home.join(".tracedecay")),
        EnvVarGuard::unset("XDG_CONFIG_HOME"),
        EnvVarGuard::unset("KIRO_HOME"),
        EnvVarGuard::unset("VIBE_HOME"),
    ]
}

#[tokio::test]
async fn fresh_install_writes_cli_fallback_rules_for_every_host() {
    let _env_lock = PROCESS_ENV_LOCK.lock().await;
    let fallback = cli_fallback_paragraph();
    assert!(
        fallback.contains("also available as a shell command"),
        "CLI-fallback paragraph lost its distinctive wording"
    );

    for case in hosts() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let _guards = env_guards(home);

        let integration = get_integration(case.id).unwrap();
        integration.install(&install_ctx(home)).unwrap();

        let path = (case.rules_path)(home);
        let contents = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "{}: rules file {} should exist after install: {e}",
                case.id,
                path.display()
            )
        });
        assert_eq!(
            contents.matches(fallback).count(),
            1,
            "{}: fresh install should write the CLI-fallback paragraph exactly once to {}",
            case.id,
            path.display()
        );
        assert!(
            contents.contains(case.marker),
            "{}: fresh install should write the managed rules marker",
            case.id
        );
    }
}

#[tokio::test]
async fn reinstall_refreshes_stale_prompt_rules_for_every_host() {
    let _env_lock = PROCESS_ENV_LOCK.lock().await;
    let fallback = cli_fallback_paragraph();
    let stale_sentinel = "Old tracedecay guidance without the CLI fallback paragraph.";

    for case in hosts() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let _guards = env_guards(home);

        let path = (case.rules_path)(home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let stale = format!(
            "## User rules before\n\nKeep this user guidance.\n\n\
             {marker}\n\n{stale_sentinel}{tail}\n\n\
             ## User rules after\n\nAlso keep this user guidance.\n",
            marker = case.marker,
            tail = case.stale_block_tail,
        );
        std::fs::write(&path, &stale).unwrap();

        let integration = get_integration(case.id).unwrap();
        integration.install(&install_ctx(home)).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            contents.matches(fallback).count(),
            1,
            "{}: reinstall over a stale block should leave exactly one CLI-fallback paragraph in {}\n---\n{contents}",
            case.id,
            path.display()
        );
        assert!(
            !contents.contains(stale_sentinel),
            "{}: stale managed block should be replaced on reinstall",
            case.id
        );
        assert_eq!(
            contents.matches(case.marker).count(),
            1,
            "{}: the managed rules marker should appear exactly once after refresh",
            case.id
        );
        assert!(
            contents.contains("Keep this user guidance.")
                && contents.contains("## User rules before"),
            "{}: user content before the managed block must be preserved",
            case.id
        );
        assert!(
            contents.contains("Also keep this user guidance.")
                && contents.contains("## User rules after"),
            "{}: user content after the managed block must be preserved",
            case.id
        );

        integration.install(&install_ctx(home)).unwrap();
        let contents_again = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            contents_again.matches(fallback).count(),
            1,
            "{}: repeated install after refresh must stay idempotent",
            case.id
        );
    }
}

/// The kiro end-marker constant this test seeds must stay in sync with the
/// integration's owned end marker.
#[tokio::test]
async fn kiro_stale_seed_uses_current_end_marker() {
    let _env_lock = PROCESS_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _guards = env_guards(home);

    get_integration("kiro")
        .unwrap()
        .install(&install_ctx(home))
        .unwrap();
    let steering = std::fs::read_to_string(home.join(".kiro/steering/tracedecay.md")).unwrap();
    assert!(
        steering.contains(KIRO_END_MARKER),
        "kiro steering should end with the owned end marker"
    );
}
