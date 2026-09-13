use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use super::Scope;
use tracedecay_domain::errors::{Result, TraceDecayError};

#[cfg(test)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DiagnosticsFingerprintOperationCounts {
    pub(super) workspace_walks: usize,
    pub(super) metadata_reads: usize,
}

#[cfg(test)]
static OPERATION_COUNTS: Mutex<Option<(PathBuf, DiagnosticsFingerprintOperationCounts)>> =
    Mutex::new(None);

#[cfg(test)]
pub(super) fn reset_operation_counts(project_root: &Path) {
    *OPERATION_COUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((
        project_root.to_path_buf(),
        DiagnosticsFingerprintOperationCounts::default(),
    ));
}

#[cfg(test)]
pub(super) fn operation_counts(project_root: &Path) -> DiagnosticsFingerprintOperationCounts {
    OPERATION_COUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|(observed_root, _)| observed_root == project_root)
        .map(|(_, counts)| counts.clone())
        .unwrap_or_default()
}

#[cfg(test)]
fn record_operation(project_root: &Path, metadata_read: bool) {
    let mut observed = OPERATION_COUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((observed_root, counts)) = observed.as_mut()
        && observed_root == project_root
    {
        if metadata_read {
            counts.metadata_reads += 1;
        } else {
            counts.workspace_walks += 1;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiagnosticsFingerprint {
    pub(super) files: Vec<DiagnosticsFileFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiagnosticsFileFingerprint {
    pub(super) path: String,
    pub(super) bytes: u64,
    pub(super) mtime_nanos: u128,
}

impl DiagnosticsFingerprint {
    #[hotpath::measure(label = "compile_diagnostics.fingerprint.capture", future = true)]
    pub(super) async fn capture(project_root: &Path, scope: &Scope) -> Result<Self> {
        let project_root = project_root.to_path_buf();
        let scope = scope.clone();
        tokio::task::spawn_blocking(move || {
            let paths = diagnostics_input_paths(&project_root, &scope)?;
            Ok(Self::from_paths(&project_root, paths))
        })
        .await
        .map_err(|err| TraceDecayError::Config {
            message: format!("failed to join diagnostics fingerprint task: {err}"),
        })?
    }

    fn from_paths(project_root: &Path, paths: Vec<PathBuf>) -> Self {
        let mut fingerprint = Self { files: Vec::new() };
        for path in paths {
            fingerprint.include_path(project_root, &path);
        }
        fingerprint
            .files
            .sort_by(|left, right| left.path.cmp(&right.path));
        fingerprint
    }

    fn include_path(&mut self, project_root: &Path, path: &Path) {
        #[cfg(test)]
        record_operation(project_root, true);
        let Ok(metadata) = std::fs::metadata(path) else {
            return;
        };
        if !metadata.is_file() {
            return;
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let relative = path.strip_prefix(project_root).unwrap_or(path);
        self.files.push(DiagnosticsFileFingerprint {
            path: relative.to_string_lossy().replace('\\', "/"),
            bytes: metadata.len(),
            mtime_nanos: modified,
        });
    }
}

fn diagnostics_input_paths(project_root: &Path, scope: &Scope) -> Result<Vec<PathBuf>> {
    match scope {
        // Cargo, tsc, and pyright all run project-level checks for file scope
        // when LSP diagnostics are unavailable; fingerprint the same inputs
        // the fallback drivers can read so post-filtered file diagnostics
        // cannot go stale after another source file changes.
        Scope::File { .. } | Scope::Workspace | Scope::Package { .. } => {
            workspace_diagnostics_input_paths(project_root)
        }
    }
}

fn workspace_diagnostics_input_paths(project_root: &Path) -> Result<Vec<PathBuf>> {
    #[cfg(test)]
    record_operation(project_root, false);
    let mut paths = Vec::new();
    for entry in walkdir::WalkDir::new(project_root)
        .into_iter()
        .filter_entry(|entry| should_walk_diagnostics_path(entry.path()))
    {
        let entry = entry.map_err(|err| TraceDecayError::Config {
            message: format!("failed to fingerprint diagnostics inputs: {err}"),
        })?;
        let path = entry.path();
        if path.is_file() && is_diagnostics_input(path) {
            paths.push(path.to_path_buf());
        }
    }
    Ok(paths)
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
    if let Some(
        "Cargo.lock" | "Cargo.toml" | "package.json" | "tsconfig.json" | "pyproject.toml"
        | "pyrightconfig.json",
    ) = path.file_name().and_then(|name| name.to_str())
    {
        return true;
    }

    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "json" | "py")
    )
}
