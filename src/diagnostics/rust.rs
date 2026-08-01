// Rust guideline compliant 2025-10-17
//! `cargo check --message-format=json` driver.
//!
//! Each `compiler-message` line in the JSON stream produces zero or more
//! `Diagnostic` rows: zero when the message has no spans (rare; usually a
//! cross-cutting note), one per `spans[]` entry otherwise.
//!
//! The cargo target dir is forced outside the project tree so concurrent
//! IDE / user `cargo check` runs don't fight us for `target/`'s lockfile
//! and diagnostics do not create repo-local `TraceDecay` folders.
//!
//! Per-package and per-file scopes drop to `cargo check -p <pkg>`; cargo
//! has no native single-file mode, so the `File` scope falls back to
//! `Workspace` and the MCP layer post-filters the results.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;

use serde::Deserialize;

use crate::diagnostics::{Diagnostic, Driver, Scope, canonicalise_file, is_diagnostic_level};
use crate::errors::{Result, TraceDecayError};

/// Driver for Rust projects. Probes for `Cargo.toml` at the project root.
pub struct CargoDriver;

impl Driver for CargoDriver {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn detect(&self, project_root: &Path) -> bool {
        project_root.join("Cargo.toml").exists()
    }

    fn run<'a>(
        &'a self,
        project_root: &'a Path,
        scope: &'a Scope,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Diagnostic>>> + Send + 'a>> {
        Box::pin(async move {
            let target_dir = target_dir_for(project_root);

            let mut cmd = tokio::process::Command::new("cargo");
            cmd.arg("check")
                .arg("--message-format=json")
                .arg("--target-dir")
                .arg(&target_dir)
                .current_dir(project_root)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);

            if let Scope::Package { name } = scope {
                cmd.arg("-p").arg(name);
            }

            let output = cmd.output().await.map_err(|e| TraceDecayError::Config {
                message: format!("failed to spawn cargo: {e}"),
            })?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut diagnostics = Vec::new();
            for line in stdout.lines() {
                if line.is_empty() {
                    continue;
                }
                let parsed: CargoLine = match serde_json::from_str(line) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                if parsed.reason != "compiler-message" {
                    continue;
                }
                let Some(msg) = parsed.message else { continue };
                if !is_diagnostic_level(&msg.level) {
                    continue;
                }
                if msg.spans.is_empty() {
                    continue;
                }

                let code = msg
                    .code
                    .as_ref()
                    .map(|c| c.code.clone())
                    .unwrap_or_default();

                for span in &msg.spans {
                    if !span.is_primary {
                        continue;
                    }
                    let rel_file = canonicalise_file(&span.file_name, project_root);
                    diagnostics.push(Diagnostic {
                        file: rel_file,
                        line_start: span.line_start,
                        line_end: span.line_end,
                        level: msg.level.clone(),
                        code: code.clone(),
                        message: msg.message.clone(),
                        driver: "rust",
                    });
                }
            }

            Ok(diagnostics)
        })
    }
}

/// Private cargo target dir for diagnostics. Created lazily by cargo on first
/// run and kept outside the project tree so diagnostics never create
/// project-local `.tracedecay` folders.
///
/// `pub(crate)` so the MCP handler can probe whether it is cold (never built)
/// and decide whether to prewarm rather than block for minutes.
/// This names a build cache, not a store, so it takes the path-local id rather
/// than the repository-collapsed one. Linked worktrees of a repository share a
/// project id but check out different sources; giving them one cargo target
/// directory would mix their artifacts and serialize them on cargo's directory
/// lock.
pub(crate) fn target_dir_for(project_root: &Path) -> PathBuf {
    std::env::temp_dir()
        .join("tracedecay-target")
        .join(crate::storage::path_local_profile_project_id(project_root))
        .join("diagnostics")
}

#[derive(Debug, Deserialize)]
struct CargoLine {
    reason: String,
    message: Option<CargoMessage>,
}

#[derive(Debug, Deserialize)]
struct CargoMessage {
    level: String,
    message: String,
    code: Option<CargoCode>,
    spans: Vec<CargoSpan>,
}

#[derive(Debug, Deserialize)]
struct CargoCode {
    code: String,
}

#[derive(Debug, Deserialize)]
struct CargoSpan {
    file_name: String,
    line_start: u32,
    line_end: u32,
    is_primary: bool,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn canonicalise_file_relative_passes_through() {
        let root = Path::new("/tmp/proj");
        assert_eq!(canonicalise_file("src/lib.rs", root), "src/lib.rs");
    }

    #[test]
    fn canonicalise_file_absolute_strips_root() {
        let root = Path::new("/tmp/proj");
        assert_eq!(
            canonicalise_file("/tmp/proj/src/lib.rs", root),
            "src/lib.rs"
        );
    }

    #[test]
    fn canonicalise_file_outside_project_passes_through() {
        let root = Path::new("/tmp/proj");
        assert_eq!(canonicalise_file("/etc/passwd", root), "/etc/passwd");
    }

    #[test]
    fn is_diagnostic_level_filters_advisory() {
        assert!(is_diagnostic_level("error"));
        assert!(is_diagnostic_level("warning"));
        assert!(!is_diagnostic_level("note"));
        assert!(!is_diagnostic_level("help"));
        assert!(!is_diagnostic_level("failure-note"));
    }

    #[test]
    fn target_dir_is_outside_project_tree() {
        let project = Path::new("/tmp/proj");
        let target = target_dir_for(project);
        assert!(target.starts_with(std::env::temp_dir().join("tracedecay-target")));
        assert!(target.ends_with("diagnostics"));
        assert!(!target.starts_with(project));
    }

    #[test]
    fn linked_worktrees_of_one_repository_get_separate_target_dirs() {
        fn git(dir: &Path, args: &[&str]) {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?} failed");
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let primary = temp.path().join("primary");
        std::fs::create_dir_all(&primary).expect("create primary");
        git(&primary, &["init", "--initial-branch=main"]);
        git(&primary, &["config", "user.email", "test@example.com"]);
        git(&primary, &["config", "user.name", "test"]);
        std::fs::write(primary.join("file.txt"), "x").expect("seed file");
        git(&primary, &["add", "file.txt"]);
        git(&primary, &["commit", "-m", "seed"]);

        let worktree = temp.path().join("linked");
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "linked",
                worktree.to_str().expect("utf-8 path"),
            ],
        );

        // The two checkouts deliberately share one store identity...
        assert_eq!(
            crate::storage::default_profile_project_id(&primary),
            crate::storage::default_profile_project_id(&worktree),
        );
        // ...but they hold different sources, so they must not share a cargo
        // target directory and contend for its lock.
        assert_ne!(target_dir_for(&primary), target_dir_for(&worktree));
    }
}
