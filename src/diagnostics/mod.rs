// Rust guideline compliant 2025-10-17
//! Compile/type-check diagnostics, normalised across languages.
//!
//! 5.0 ships the Rust driver (`cargo check --message-format=json`) — the
//! largest single Bash:tracedecay gap in the 2026-05-04 telemetry scan
//! (777 invocations). TypeScript (`tsc --noEmit`) and Python (`pyright`)
//! drivers land in follow-up commits.
//!
//! The contract is that every driver returns a `Vec<Diagnostic>` with a
//! consistent shape. The MCP layer enriches each diagnostic with the
//! enclosing graph node, so callers get structured errors mapped to the
//! same node IDs the rest of tracedecay's tools speak.

pub mod lsp;
pub mod python;
pub mod rust;
pub mod typescript;

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;

use crate::errors::{Result, TraceDecayError};

/// One diagnostic emitted by a language's type-checker.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    /// Project-relative file path (e.g. `src/lib.rs`).
    pub file: String,
    /// 1-based start line.
    pub line_start: u32,
    /// 1-based inclusive end line. Equal to `line_start` for single-line spans.
    pub line_end: u32,
    /// Severity. Common values: `"error"`, `"warning"`, `"note"`. Drivers
    /// pass through whatever the compiler reports.
    pub level: String,
    /// Compiler-assigned code (e.g. `"E0308"` for Rust, `"7053"` for TS).
    /// Empty when the compiler didn't attach one.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Driver source — `"rust"`, `"typescript"`, etc. Useful when a project
    /// runs multiple drivers in one pass.
    pub driver: &'static str,
}

/// Scope of the diagnostic run. Drivers may not honor every scope; the
/// `Workspace` variant is the universal fallback.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Whole workspace / project. The default and most expensive scope.
    Workspace,
    /// A single package / cargo crate / TypeScript project root.
    Package { name: String },
    /// A single file. Most useful for editor-style on-save checks.
    File { path: String },
}

#[derive(Debug, Default)]
pub struct DiagnosticsCache {
    entries: tokio::sync::Mutex<HashMap<DiagnosticsCacheKey, CachedDiagnostics>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DiagnosticsCacheKey {
    project_root: PathBuf,
    scope: Scope,
}

#[derive(Debug, Clone)]
struct CachedDiagnostics {
    fingerprint: DiagnosticsFingerprint,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiagnosticsFingerprint {
    files: u64,
    bytes: u64,
    newest_mtime_nanos: u128,
}

impl DiagnosticsCache {
    pub async fn run(&self, project_root: &Path, scope: &Scope) -> Result<Vec<Diagnostic>> {
        self.run_with(project_root, scope, || run_all(project_root, scope))
            .await
    }

    async fn run_with<F, Fut>(
        &self,
        project_root: &Path,
        scope: &Scope,
        run: F,
    ) -> Result<Vec<Diagnostic>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<Diagnostic>>>,
    {
        let key = DiagnosticsCacheKey {
            project_root: project_root
                .canonicalize()
                .unwrap_or_else(|_| project_root.to_path_buf()),
            scope: scope.clone(),
        };
        let fingerprint = DiagnosticsFingerprint::capture(project_root, scope)?;
        let mut entries = self.entries.lock().await;
        if let Some(cached) = entries.get(&key) {
            if cached.fingerprint == fingerprint {
                return Ok(cached.diagnostics.clone());
            }
        }

        let diagnostics = run().await?;
        entries.insert(
            key,
            CachedDiagnostics {
                fingerprint,
                diagnostics: diagnostics.clone(),
            },
        );
        Ok(diagnostics)
    }
}

impl DiagnosticsFingerprint {
    fn capture(project_root: &Path, scope: &Scope) -> Result<Self> {
        let mut fingerprint = Self {
            files: 0,
            bytes: 0,
            newest_mtime_nanos: 0,
        };

        match scope {
            Scope::File { path } => {
                fingerprint.include_path(&project_root.join(path))?;
            }
            Scope::Workspace | Scope::Package { .. } => {
                for entry in walkdir::WalkDir::new(project_root)
                    .into_iter()
                    .filter_entry(|entry| should_walk_diagnostics_path(entry.path()))
                {
                    let entry = entry.map_err(|err| TraceDecayError::Config {
                        message: format!("failed to fingerprint diagnostics inputs: {err}"),
                    })?;
                    let path = entry.path();
                    if path.is_file() && is_diagnostics_input(path) {
                        fingerprint.include_path(path)?;
                    }
                }
            }
        }

        Ok(fingerprint)
    }

    fn include_path(&mut self, path: &Path) -> Result<()> {
        let Ok(metadata) = std::fs::metadata(path) else {
            return Ok(());
        };
        self.files = self.files.saturating_add(1);
        self.bytes = self.bytes.saturating_add(metadata.len());
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        self.newest_mtime_nanos = self.newest_mtime_nanos.max(modified);
        Ok(())
    }
}

fn should_walk_diagnostics_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    !matches!(
        name,
        ".git" | ".tracedecay" | ".worktrees" | "target" | "node_modules" | "dist"
    )
}

fn is_diagnostics_input(path: &Path) -> bool {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(
            "Cargo.lock" | "Cargo.toml" | "package.json" | "tsconfig.json" | "pyproject.toml"
            | "pyrightconfig.json",
        ) => return true,
        _ => {}
    }

    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "json" | "py")
    )
}

/// Per-language driver contract. Implementations live in submodules
/// (`rust`, `typescript`, `python`, …).
pub trait Driver {
    /// Driver identifier (`"rust"`, `"typescript"`, `"python"`).
    fn name(&self) -> &'static str;

    /// True when `project_root` looks like the kind of project this driver
    /// handles. Cheap probe — typically existence of a manifest file.
    fn detect(&self, project_root: &Path) -> bool;

    /// Run the diagnostic pass over `scope`. Implementations are async
    /// because they shell out to the compiler.
    fn run<'a>(
        &'a self,
        project_root: &'a Path,
        scope: &'a Scope,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<Diagnostic>>> + Send + 'a>>;
}

/// Run every detected driver against `project_root` and return the merged
/// diagnostic list. Drivers are run sequentially; any driver-level error
/// is propagated immediately. Empty when no driver detects the project.
pub async fn run_all(project_root: &Path, scope: &Scope) -> Result<Vec<Diagnostic>> {
    let drivers: Vec<Box<dyn Driver + Send + Sync>> = vec![
        Box::new(rust::CargoDriver),
        Box::new(typescript::TscDriver),
        Box::new(python::PyrightDriver),
    ];

    let mut all = Vec::new();
    for driver in drivers {
        if !driver.detect(project_root) {
            continue;
        }
        let mut diags = driver.run(project_root, scope).await?;
        all.append(&mut diags);
    }
    Ok(all)
}

/// Whether the Rust diagnostics build is "cold" — i.e. the private cargo
/// target dir does not exist yet or is effectively empty. The first
/// `cargo check` against a cold tree builds every dependency from scratch and
/// can block for minutes on a large workspace; the MCP layer uses this to
/// offer a non-blocking prewarm instead of freezing the agent.
///
/// Returns `false` for projects without a `Cargo.toml` (nothing to warm) and
/// once the target dir has any build artifacts (subsequent checks are fast).
pub fn is_rust_diagnostics_cold(project_root: &Path) -> bool {
    if !project_root.join("Cargo.toml").exists() {
        return false;
    }
    let target_dir = rust::target_dir_for(project_root);
    dir_is_missing_or_empty(&target_dir)
}

/// True when `path` does not exist, is not a directory, or contains no
/// entries. Any read error is treated as "cold" so we err toward prewarming
/// rather than blocking.
fn dir_is_missing_or_empty(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => true,
    }
}

/// Path to the private Rust diagnostics cargo target dir. Exposed so callers
/// can surface it in prewarm status messages.
pub fn rust_diagnostics_target_dir(project_root: &Path) -> PathBuf {
    rust::target_dir_for(project_root)
}

/// Spawn a detached `cargo check` into the private diagnostics target dir so a
/// later `tracedecay_diagnostics` call finds a warm tree. Returns immediately;
/// the child keeps running after this process would normally reap it (stdio is
/// discarded and it is intentionally NOT `kill_on_drop`, unlike the foreground
/// driver, so it survives the request that started it).
pub fn spawn_rust_diagnostics_prewarm(project_root: &Path) -> Result<()> {
    use std::process::{Command, Stdio};

    let target_dir = rust::target_dir_for(project_root);
    if let Some(parent) = target_dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    Command::new("cargo")
        .arg("check")
        .arg("--message-format=json")
        .arg("--target-dir")
        .arg(&target_dir)
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_child| ())
        .map_err(|e| crate::errors::TraceDecayError::Config {
            message: format!("failed to spawn cargo prewarm: {e}"),
        })
}

fn canonicalise_file(file_name: &str, project_root: &Path) -> String {
    let abs = if Path::new(file_name).is_absolute() {
        PathBuf::from(file_name)
    } else {
        project_root.join(file_name)
    };
    if let Ok(rel) = abs.strip_prefix(project_root) {
        return rel.to_string_lossy().to_string();
    }
    file_name.to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    fn test_diagnostic(message: &str) -> Diagnostic {
        Diagnostic {
            file: "src/lib.rs".to_string(),
            line_start: 1,
            line_end: 1,
            level: "error".to_string(),
            code: "E0000".to_string(),
            message: message.to_string(),
            driver: "test",
        }
    }

    #[tokio::test]
    async fn cache_single_flights_concurrent_identical_requests() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn demo() {}\n").unwrap();

        let cache = Arc::new(DiagnosticsCache::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Barrier::new(2));

        let mut tasks = Vec::new();
        for _ in 0..2 {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            let gate = Arc::clone(&gate);
            let project_root = temp.path().to_path_buf();
            tasks.push(tokio::spawn(async move {
                gate.wait().await;
                cache
                    .run_with(&project_root, &Scope::Workspace, || {
                        let calls = Arc::clone(&calls);
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            Ok(vec![test_diagnostic("cached")])
                        }
                    })
                    .await
                    .unwrap()
            }));
        }

        let first = tasks.remove(0).await.unwrap();
        let second = tasks.remove(0).await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first[0].message, "cached");
        assert_eq!(second[0].message, "cached");
    }

    #[tokio::test]
    async fn cache_invalidates_when_inputs_change() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        let source = temp.path().join("src/lib.rs");
        std::fs::write(&source, "pub fn demo() {}\n").unwrap();

        let cache = DiagnosticsCache::default();
        let calls = Arc::new(AtomicUsize::new(0));

        for message in ["first", "second"] {
            let calls = Arc::clone(&calls);
            let diagnostics = cache
                .run_with(temp.path(), &Scope::Workspace, || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(vec![test_diagnostic(message)])
                })
                .await
                .unwrap();
            assert_eq!(diagnostics[0].message, message);
            std::fs::write(&source, format!("pub fn demo() {{}}\n// {message}\n")).unwrap();
        }

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
