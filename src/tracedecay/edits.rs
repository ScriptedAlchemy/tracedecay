//! Anchored source-editing primitives (str-replace, insert, symbol
//! replacement, ast-grep rewrites) plus the single-file re-index they
//! trigger.
//!
//! Direct graph mutations are crate-internal adapters; external callers must
//! use the canonical source-edit transaction.
//!
//! ```compile_fail
//! async fn direct_str_replace_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph.str_replace("src/lib.rs", "old", "new", true).await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_multi_str_replace_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph
//!         .multi_str_replace("src/lib.rs", &[("old", "new")], true)
//!         .await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_insert_at_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph
//!         .insert_at("src/lib.rs", "anchor", "content", true, true)
//!         .await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_replace_symbol_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph.replace_symbol("symbol", "fn symbol() {}", true).await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_insert_at_symbol_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph
//!         .insert_at_symbol("symbol", "content", "before", true)
//!         .await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_ast_grep_rewrite_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph
//!         .ast_grep_rewrite("src/lib.rs", "$A", "$A", true)
//!         .await;
//! }
//! ```
//! ```compile_fail
//! async fn direct_move_symbol_is_not_public(graph: &tracedecay::tracedecay::TraceDecay) {
//!     let _ = graph
//!         .move_symbol("symbol", "src/dest.rs", true, false)
//!         .await;
//! }
//! ```

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsString;
use std::future::Future;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, ambient_authority};
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use same_file::Handle;
use serde::{Deserialize, Serialize};

use crate::errors::{Result, TraceDecayError};
use crate::sync;
use crate::types::*;

use super::indexing::{accumulate_symbol_scope, safe_extract};
use super::{TraceDecay, current_timestamp};

static SOURCE_EDIT_TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

struct SourceEditFileAuthority {
    root: Dir,
    parent: Dir,
    parent_relative: PathBuf,
    name: OsString,
}

impl SourceEditFileAuthority {
    fn open(project_root: &Path, relative: &Path) -> Result<Self> {
        let relative = normalize_source_edit_relative_path(relative)?;
        let root = Dir::open_ambient_dir(project_root, ambient_authority())
            .map_err(|error| source_edit_path_error("open authorized source edit root", error))?;
        let components = relative.components().collect::<Vec<_>>();
        let Some(Component::Normal(name)) = components.last() else {
            return Err(source_edit_unsafe_path());
        };
        let mut parent = root
            .open_dir_nofollow(".")
            .map_err(|error| source_edit_path_error("open source edit root", error))?;
        let mut parent_relative = PathBuf::new();
        for component in &components[..components.len().saturating_sub(1)] {
            let Component::Normal(component) = component else {
                return Err(source_edit_unsafe_path());
            };
            parent = parent.open_dir_nofollow(component).map_err(|error| {
                source_edit_path_error("open source edit parent without following symlinks", error)
            })?;
            parent_relative.push(component);
        }
        Ok(Self {
            root,
            parent,
            parent_relative,
            name: name.to_os_string(),
        })
    }

    fn open_optional(&self) -> Result<Option<cap_std::fs::File>> {
        match self.parent.symlink_metadata(&self.name) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => return Err(source_edit_unsafe_path()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(source_edit_path_error(
                    "inspect source edit candidate",
                    error,
                ));
            }
        }
        let mut options = CapOpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let input = self
            .parent
            .open_with(&self.name, &options)
            .map_err(|error| {
                source_edit_path_error(
                    "open source edit candidate without following symlinks",
                    error,
                )
            })?;
        if !input
            .metadata()
            .map_err(|error| source_edit_path_error("inspect opened source edit candidate", error))?
            .is_file()
        {
            return Err(source_edit_unsafe_path());
        }
        Ok(Some(input))
    }

    fn read_optional_with_identity(&self) -> Result<(Option<Vec<u8>>, Option<Handle>)> {
        let Some(mut input) = self.open_optional()? else {
            return Ok((None, None));
        };
        let identity = Handle::from_file(
            input
                .try_clone()
                .map_err(|error| source_edit_path_error("clone source edit candidate", error))?
                .into_std(),
        )
        .map_err(|error| source_edit_path_error("identify source edit candidate", error))?;
        let mut bytes = Vec::new();
        input
            .read_to_end(&mut bytes)
            .map_err(|error| source_edit_path_error("read source edit candidate", error))?;
        let current = self
            .current_identity()?
            .ok_or_else(source_edit_unsafe_path)?;
        if current != identity {
            return Err(TraceDecayError::Config {
                message: "source edit candidate changed while it was read".to_owned(),
            });
        }
        Ok((Some(bytes), Some(identity)))
    }

    fn read_optional(&self) -> Result<Option<Vec<u8>>> {
        self.read_optional_with_identity().map(|(bytes, _)| bytes)
    }

    fn read_to_string(&self, label: &str) -> Result<(String, Handle)> {
        let (bytes, identity) = self.read_optional_with_identity()?;
        let bytes = bytes.ok_or_else(|| TraceDecayError::Config {
            message: format!("failed to read {label}: file was not found"),
        })?;
        let source = String::from_utf8(bytes).map_err(|error| TraceDecayError::Config {
            message: format!("failed to read {label}: {error}"),
        })?;
        Ok((
            source,
            identity.expect("present source edit bytes have an opened file identity"),
        ))
    }

    fn current_identity(&self) -> Result<Option<Handle>> {
        self.open_optional()?
            .map(|file| {
                Handle::from_file(file.into_std()).map_err(|error| {
                    source_edit_path_error("identify current source edit candidate", error)
                })
            })
            .transpose()
    }

    fn verify_parent_binding(&self) -> Result<()> {
        let current = if self.parent_relative.as_os_str().is_empty() {
            self.root.open_dir_nofollow(".")
        } else {
            let mut current = self.root.open_dir_nofollow(".");
            for component in self.parent_relative.components() {
                let Component::Normal(component) = component else {
                    return Err(source_edit_unsafe_path());
                };
                current = current.and_then(|directory| directory.open_dir_nofollow(component));
            }
            current
        }
        .map_err(|error| {
            source_edit_path_error(
                "revalidate source edit parent without following symlinks",
                error,
            )
        })?;
        let expected = Handle::from_file(
            self.parent
                .try_clone()
                .map_err(|error| source_edit_path_error("clone source edit parent", error))?
                .into_std_file(),
        )
        .map_err(|error| source_edit_path_error("identify source edit parent", error))?;
        let observed = Handle::from_file(current.into_std_file()).map_err(|error| {
            source_edit_path_error("identify rebound source edit parent", error)
        })?;
        if expected != observed {
            return Err(TraceDecayError::Config {
                message: "source edit parent changed before atomic publication".to_owned(),
            });
        }
        Ok(())
    }

    fn publish(
        &self,
        relative_path: &str,
        expected: Option<&str>,
        expected_identity: Option<&Handle>,
        intended: &str,
        before_compare: impl FnOnce(),
    ) -> Result<()> {
        self.verify_parent_binding()?;
        // Capture the candidate's current permission bits so the atomic replace
        // does not silently downgrade them. The temporary is still created
        // 0o600 so it is never briefly more permissive than the final file; the
        // original mode is restored on the open handle below.
        #[cfg(unix)]
        let published_mode = {
            use cap_std::fs::PermissionsExt;
            self.metadata()
                .ok()
                .map(|metadata| metadata.permissions().mode())
        };
        let mut before_compare = Some(before_compare);
        for _ in 0..64 {
            let temporary = OsString::from(format!(
                ".tracedecay-source-edit.{}.{}.tmp",
                std::process::id(),
                SOURCE_EDIT_TEMP_NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = CapOpenOptions::new();
            options
                .write(true)
                .create_new(true)
                .follow(FollowSymlinks::No);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut output = match self.parent.open_with(&temporary, &options) {
                Ok(output) => output,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(source_edit_path_error(
                        "create source edit temporary file",
                        error,
                    ));
                }
            };
            let result = (|| {
                output.write_all(intended.as_bytes()).map_err(|error| {
                    source_edit_path_error("write source edit temporary file", error)
                })?;
                // fchmod on the open handle rather than the path: it is immune
                // to umask (unlike the create mode) and cannot race a swap of
                // the temporary name.
                #[cfg(unix)]
                if let Some(mode) = published_mode {
                    use cap_std::fs::{Permissions, PermissionsExt};
                    output
                        .set_permissions(Permissions::from_mode(mode))
                        .map_err(|error| {
                            source_edit_path_error("preserve source edit permissions", error)
                        })?;
                }
                output.sync_all().map_err(|error| {
                    source_edit_path_error("sync source edit temporary file", error)
                })?;
                drop(output);
                before_compare
                    .take()
                    .expect("source edit comparison hook runs once")();
                self.verify_parent_binding()?;
                let (current, current_identity) = self.read_optional_with_identity()?;
                if current.as_deref() != expected.map(str::as_bytes) {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "source edit candidate {relative_path} changed before atomic publication"
                        ),
                    });
                }
                if current_identity.as_ref() != expected_identity {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "source edit candidate {relative_path} was replaced before atomic publication"
                        ),
                    });
                }
                self.parent
                    .rename(&temporary, &self.parent, &self.name)
                    .map_err(|error| {
                        source_edit_path_error("atomically publish source edit candidate", error)
                    })?;
                sync_source_edit_directory(&self.parent)
            })();
            if result.is_err() {
                let _ = self.parent.remove_file(&temporary);
            }
            return result;
        }
        Err(TraceDecayError::Config {
            message: "could not allocate source edit temporary file".to_owned(),
        })
    }

    fn metadata(&self) -> Result<cap_std::fs::Metadata> {
        self.parent
            .symlink_metadata(&self.name)
            .map_err(|error| source_edit_path_error("inspect source edit candidate", error))
    }
}

pub(crate) fn read_source_edit_candidate(
    project_root: &Path,
    relative: &Path,
) -> Result<Option<Vec<u8>>> {
    SourceEditFileAuthority::open(project_root, relative)?.read_optional()
}

pub(crate) fn validate_source_edit_candidate_parent(
    project_root: &Path,
    relative: &Path,
) -> Result<()> {
    SourceEditFileAuthority::open(project_root, relative).map(|_| ())
}

fn normalize_source_edit_relative_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(source_edit_unsafe_path());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            Component::CurDir => {}
            _ => return Err(source_edit_unsafe_path()),
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(source_edit_unsafe_path());
    }
    Ok(normalized)
}

fn source_edit_unsafe_path() -> TraceDecayError {
    TraceDecayError::Config {
        message: "source edit path is not a regular file beneath the authorized worktree"
            .to_owned(),
    }
}

fn source_edit_path_error(operation: &'static str, error: io::Error) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("{operation}: {error}"),
    }
}

fn sync_source_edit_directory(directory: &Dir) -> Result<()> {
    #[cfg(windows)]
    {
        directory
            .dir_metadata()
            .map(|_| ())
            .map_err(|error| source_edit_path_error("sync source edit parent directory", error))
    }
    #[cfg(not(windows))]
    {
        let mut options = CapOpenOptions::new();
        options.read(true).maybe_dir(true);
        directory
            .open_with(".", &options)
            .and_then(|file| file.sync_all())
            .map_err(|error| source_edit_path_error("sync source edit parent directory", error))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlannedSourceEditFile {
    pub relative_path: String,
    pub expected: Option<String>,
    pub intended: Option<String>,
}

#[derive(Debug)]
struct SourceEditApplyState {
    files: Vec<PlannedSourceEditFile>,
    consumed: BTreeSet<String>,
}

tokio::task_local! {
    static SOURCE_EDIT_PLAN_CAPTURE: Arc<Mutex<Vec<PlannedSourceEditFile>>>;
    static SOURCE_EDIT_APPLY_STATE: Arc<Mutex<SourceEditApplyState>>;
}

pub(crate) async fn capture_source_edit_plan<T>(
    future: impl Future<Output = T>,
) -> (T, Vec<PlannedSourceEditFile>) {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let result = SOURCE_EDIT_PLAN_CAPTURE
        .scope(Arc::clone(&capture), future)
        .await;
    let files = capture.lock().expect("source edit plan lock").clone();
    (result, files)
}

pub(crate) async fn apply_source_edit_plan<T>(
    files: Vec<PlannedSourceEditFile>,
    future: impl Future<Output = T>,
) -> (T, bool) {
    let expected_count = files.len();
    let state = Arc::new(Mutex::new(SourceEditApplyState {
        files,
        consumed: BTreeSet::new(),
    }));
    let result = SOURCE_EDIT_APPLY_STATE
        .scope(Arc::clone(&state), future)
        .await;
    let complete = state.lock().expect("source edit apply lock").consumed.len() == expected_count;
    (result, complete)
}

pub(crate) fn rollback_planned_source_edit_files(
    project_root: &Path,
    files: &[PlannedSourceEditFile],
) -> Result<()> {
    let observed = files
        .iter()
        .map(|file| {
            let current = read_source_edit_candidate(project_root, Path::new(&file.relative_path))?;
            let expected = file.expected.as_deref().map(str::as_bytes);
            let intended = file.intended.as_deref().map(str::as_bytes);
            if current.as_deref() != expected && current.as_deref() != intended {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "source edit crash recovery refused foreign bytes in {}",
                        file.relative_path
                    ),
                });
            }
            Ok(current)
        })
        .collect::<Result<Vec<_>>>()?;
    for (file, current) in files.iter().zip(observed).rev() {
        if current.as_deref() == file.expected.as_deref().map(str::as_bytes) {
            continue;
        }
        let (Some(current), Some(expected)) = (file.intended.as_deref(), file.expected.as_deref())
        else {
            return Err(TraceDecayError::Config {
                message: format!(
                    "source edit crash recovery cannot restore a created or removed file: {}",
                    file.relative_path
                ),
            });
        };
        publish_planned_source_edit(project_root, &file.relative_path, Some(current), expected)?;
    }
    Ok(())
}

pub(super) fn capture_planned_source_edit(
    relative_path: &str,
    expected: Option<&str>,
    intended: Option<&str>,
) {
    let _ = SOURCE_EDIT_PLAN_CAPTURE.try_with(|capture| {
        capture
            .lock()
            .expect("source edit plan lock")
            .push(PlannedSourceEditFile {
                relative_path: relative_path.to_owned(),
                expected: expected.map(str::to_owned),
                intended: intended.map(str::to_owned),
            });
    });
}

pub(super) fn validate_planned_source_edit(
    relative_path: &str,
    expected: Option<&str>,
    intended: Option<&str>,
) -> Result<()> {
    SOURCE_EDIT_APPLY_STATE
        .try_with(|state| {
            let mut state = state.lock().expect("source edit apply lock");
            let Some(planned) = state
                .files
                .iter()
                .find(|file| file.relative_path == relative_path)
            else {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "source edit apply produced unplanned candidate {relative_path}"
                    ),
                });
            };
            if planned.expected.as_deref() != expected || planned.intended.as_deref() != intended {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "source edit candidate {relative_path} drifted from its exact preview"
                    ),
                });
            }
            state.consumed.insert(relative_path.to_owned());
            Ok(())
        })
        .unwrap_or(Ok(()))
}

pub(super) fn publish_planned_source_edit(
    project_root: &Path,
    relative_path: &str,
    expected: Option<&str>,
    intended: &str,
) -> Result<()> {
    validate_planned_source_edit(relative_path, expected, Some(intended))?;
    let file = SourceEditFileAuthority::open(project_root, Path::new(relative_path))?;
    let expected_identity = file.current_identity()?;
    file.publish(
        relative_path,
        expected,
        expected_identity.as_ref(),
        intended,
        || {},
    )
}

impl TraceDecay {
    pub(crate) async fn recover_source_edit_preimages(
        &self,
        files: &[PlannedSourceEditFile],
    ) -> Result<()> {
        rollback_planned_source_edit_files(&self.project_root, files)?;
        for file in files {
            let Some(expected) = &file.expected else {
                continue;
            };
            let authority =
                SourceEditFileAuthority::open(&self.project_root, Path::new(&file.relative_path))?;
            self.reindex_file(&file.relative_path, expected, &authority)
                .await?;
        }
        Ok(())
    }

    /// Applies one immutable API-migration file family through the source-edit
    /// CAS authority. Every candidate is captured during preview. Real apply
    /// validates every preimage before the first write and restores all
    /// published files if cancellation or a later publication fails.
    pub(crate) async fn apply_api_migration_plan(
        &self,
        plan: &tracedecay_application::ApiMigrationPlanV1,
        dry_run: bool,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<tracedecay_application::ApiMigrationApplyResultV1> {
        plan.validate().map_err(|error| TraceDecayError::Config {
            message: format!("invalid API migration plan: {error}"),
        })?;
        if plan.blocked {
            return Ok(tracedecay_application::ApiMigrationApplyResultV1 {
                success: false,
                dry_run,
                family_id: plan.family_id.clone(),
                plan_digest: plan.plan_digest.clone(),
                changed_files: Vec::new(),
                changed_sites: 0,
                compatibility_sites: 0,
                protected_values_verified: 0,
                rolled_back: false,
                message: "API migration plan contains blocked sites".to_owned(),
            });
        }
        let current_revision = {
            let repository =
                gix::open(&self.project_root).map_err(|error| TraceDecayError::Config {
                    message: format!("cannot revalidate API migration repository: {error}"),
                })?;
            repository
                .head_commit()
                .map(|commit| commit.id().to_hex().to_string())
                .map_err(|error| TraceDecayError::Config {
                    message: format!("cannot revalidate API migration HEAD: {error}"),
                })?
        };
        if current_revision != plan.repository_revision {
            return Err(TraceDecayError::Config {
                message: "API migration repository revision is stale; replan before apply"
                    .to_owned(),
            });
        }

        for candidate in &plan.files {
            let observed =
                read_source_edit_candidate(&self.project_root, Path::new(&candidate.path))?
                    .ok_or_else(|| TraceDecayError::Config {
                        message: format!("API migration candidate disappeared: {}", candidate.path),
                    })?;
            if observed != candidate.expected_content.as_bytes() {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "API migration candidate {} is stale; replan before apply",
                        candidate.path
                    ),
                });
            }
            capture_planned_source_edit(
                &candidate.path,
                Some(&candidate.expected_content),
                Some(&candidate.intended_content),
            );
        }

        let changed_files = plan
            .files
            .iter()
            .filter(|file| file.expected_content != file.intended_content)
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let changed_sites = plan
            .sites
            .iter()
            .filter(|site| {
                site.disposition == tracedecay_application::ApiMigrationSiteDispositionV1::Changed
            })
            .count();
        let compatibility_operations = plan
            .operations
            .iter()
            .filter_map(|operation| match operation {
                tracedecay_application::ApiMigrationOperationRequestV1::InsertCompatibility {
                    operation_id,
                    ..
                } => Some(operation_id.as_str()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let compatibility_sites = plan
            .sites
            .iter()
            .filter(|site| compatibility_operations.contains(site.operation_id.as_str()))
            .count();
        let protected_operations = plan
            .operations
            .iter()
            .filter_map(|operation| match operation {
                tracedecay_application::ApiMigrationOperationRequestV1::AssertStableValue {
                    operation_id,
                    ..
                } => Some(operation_id.as_str()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let protected_values_verified = plan
            .sites
            .iter()
            .filter(|site| protected_operations.contains(site.operation_id.as_str()))
            .count();
        if dry_run {
            return Ok(tracedecay_application::ApiMigrationApplyResultV1 {
                success: true,
                dry_run: true,
                family_id: plan.family_id.clone(),
                plan_digest: plan.plan_digest.clone(),
                changed_files,
                changed_sites,
                compatibility_sites,
                protected_values_verified,
                rolled_back: false,
                message: "API migration dry-run revalidated the immutable plan; no files changed"
                    .to_owned(),
            });
        }

        let mut published = Vec::<&tracedecay_application::ApiMigrationFilePlanV1>::new();
        for candidate in &plan.files {
            if is_cancelled() {
                rollback_api_migration_files(&self.project_root, &published)?;
                return Ok(tracedecay_application::ApiMigrationApplyResultV1 {
                    success: false,
                    dry_run: false,
                    family_id: plan.family_id.clone(),
                    plan_digest: plan.plan_digest.clone(),
                    changed_files: Vec::new(),
                    changed_sites: 0,
                    compatibility_sites,
                    protected_values_verified,
                    rolled_back: true,
                    message: "API migration cancelled; every published file was restored"
                        .to_owned(),
                });
            }
            if candidate.expected_content == candidate.intended_content {
                validate_planned_source_edit(
                    &candidate.path,
                    Some(&candidate.expected_content),
                    Some(&candidate.intended_content),
                )?;
                continue;
            }
            if let Err(error) = publish_planned_source_edit(
                &self.project_root,
                &candidate.path,
                Some(&candidate.expected_content),
                &candidate.intended_content,
            ) {
                rollback_api_migration_files(&self.project_root, &published)?;
                return Err(TraceDecayError::Config {
                    message: format!(
                        "API migration publication failed and prior files were restored: {error}"
                    ),
                });
            }
            published.push(candidate);
        }

        for candidate in &published {
            let file =
                SourceEditFileAuthority::open(&self.project_root, Path::new(&candidate.path))?;
            if let Err(error) = self
                .reindex_file(&candidate.path, &candidate.intended_content, &file)
                .await
            {
                rollback_api_migration_files(&self.project_root, &published)?;
                for restored in &published {
                    if let Ok(file) =
                        SourceEditFileAuthority::open(&self.project_root, Path::new(&restored.path))
                    {
                        let _ = self
                            .reindex_file(&restored.path, &restored.expected_content, &file)
                            .await;
                    }
                }
                return Err(TraceDecayError::Config {
                    message: format!(
                        "API migration graph refresh failed and workspace bytes were restored: {error}"
                    ),
                });
            }
        }
        Ok(tracedecay_application::ApiMigrationApplyResultV1 {
            success: true,
            dry_run: false,
            family_id: plan.family_id.clone(),
            plan_digest: plan.plan_digest.clone(),
            changed_files,
            changed_sites,
            compatibility_sites,
            protected_values_verified,
            rolled_back: false,
            message: "API migration applied atomically and refreshed graph evidence".to_owned(),
        })
    }

    pub(crate) async fn rollback_api_migration_plan(
        &self,
        plan: &tracedecay_application::ApiMigrationPlanV1,
    ) -> Result<()> {
        let published = plan
            .files
            .iter()
            .filter(|file| file.expected_content != file.intended_content)
            .collect::<Vec<_>>();
        rollback_api_migration_files(&self.project_root, &published)?;
        for restored in published {
            let file =
                SourceEditFileAuthority::open(&self.project_root, Path::new(&restored.path))?;
            self.reindex_file(&restored.path, &restored.expected_content, &file)
                .await?;
        }
        Ok(())
    }
}

fn rollback_api_migration_files(
    project_root: &Path,
    published: &[&tracedecay_application::ApiMigrationFilePlanV1],
) -> Result<()> {
    for candidate in published.iter().rev() {
        let file = SourceEditFileAuthority::open(project_root, Path::new(&candidate.path))?;
        let (_, current_identity) = file.read_optional_with_identity()?;
        file.publish(
            &candidate.path,
            Some(&candidate.intended_content),
            current_identity.as_ref(),
            &candidate.expected_content,
            || {},
        )?;
    }
    Ok(())
}

impl TraceDecay {
    /// Resolves a path to a relative path string.
    /// If the path is already relative, validates that it stays in the project.
    /// If absolute, strips the `project_root` prefix.
    fn resolve_path(&self, path: &str) -> Option<String> {
        let path = Path::new(path);
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.project_root).ok()?
        } else {
            path
        };
        normalize_source_edit_relative_path(relative)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
    }

    /// Re-indexes a single file after an edit.
    async fn reindex_file(
        &self,
        file_path: &str,
        source: &str,
        file: &SourceEditFileAuthority,
    ) -> Result<()> {
        let Some(extractor) = self.registry.extractor_for_file(file_path) else {
            return Ok(());
        };

        let mut result =
            safe_extract(extractor, file_path, source).ok_or_else(|| TraceDecayError::Config {
                message: format!("extraction panicked for {file_path}"),
            })?;
        result.sanitize();

        let hash = sync::content_hash(source);
        let size = source.len() as u64;
        let mtime = file
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| {
                modified
                    .into_std()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_secs() as i64)
            })
            .unwrap_or_else(current_timestamp);

        let transaction = self.db.begin_write_transaction("reindex file").await?;
        self.db
            .delete_nodes_by_file_unguarded(&transaction, file_path)
            .await?;
        self.db
            .insert_nodes_unguarded(&transaction, &result.nodes)
            .await?;
        self.db
            .insert_edges_unguarded(&transaction, &result.edges)
            .await?;
        if !result.unresolved_refs.is_empty() {
            self.db
                .insert_unresolved_refs_unguarded(&transaction, &result.unresolved_refs)
                .await?;
        }

        let file_record = FileRecord {
            path: file_path.to_string(),
            content_hash: hash,
            size,
            modified_at: mtime,
            indexed_at: current_timestamp(),
            node_count: result.nodes.len() as u32,
        };
        self.db
            .upsert_file_unguarded(&transaction, &file_record)
            .await?;
        transaction.commit().await?;
        let mut short = HashSet::new();
        let mut keys = HashSet::new();
        accumulate_symbol_scope(&result.nodes, &mut short, &mut keys);
        self.reresolve_after_reindex(&[file_path.to_string()], &short, &keys)
            .await?;

        Ok(())
    }

    /// Write-or-preview gate shared by every edit primitive. On a real run this
    /// writes `modified` to `abs_path` and reindexes the file, returning `None`.
    /// On a dry run it writes nothing and reindexes nothing, instead returning a
    /// bounded preview diff of the changed region (the would-be change) so
    /// callers can review before committing. Centralizing the write here keeps
    /// the dry-run gate in one place around each primitive's own validation and
    /// span logic.
    async fn commit_or_preview_edit(
        &self,
        rel_path: &str,
        file: &SourceEditFileAuthority,
        expected_identity: &Handle,
        original: &str,
        modified: &str,
        dry_run: bool,
    ) -> Result<Option<String>> {
        if dry_run {
            capture_planned_source_edit(rel_path, Some(original), Some(modified));
            return Ok(Some(bounded_region_diff(
                original,
                modified,
                PREVIEW_DIFF_CONTEXT,
                MAX_PREVIEW_DIFF_LINES,
            )));
        }
        validate_planned_source_edit(rel_path, Some(original), Some(modified))?;
        file.publish(
            rel_path,
            Some(original),
            Some(expected_identity),
            modified,
            || {},
        )?;
        self.reindex_file(rel_path, modified, file).await?;
        Ok(None)
    }

    /// Performs a single string replacement.
    /// Fails if `old_str` is not found or matches more than once.
    pub(crate) async fn str_replace(
        &self,
        path: &str,
        old_str: &str,
        new_str: &str,
        dry_run: bool,
    ) -> Result<EditResult> {
        let rel_path = self
            .resolve_path(path)
            .ok_or_else(|| TraceDecayError::Config {
                message: "path is not within the project".to_string(),
            })?;

        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(path)?;

        let matches: Vec<_> = source.match_indices(old_str).collect();
        match matches.len() {
            0 => {
                return Ok(EditResult {
                    success: false,
                    file_path: rel_path.clone(),
                    matched_str: old_str.to_string(),
                    new_str: new_str.to_string(),
                    replaced_span: None,
                    dry_run,
                    diff: None,
                    message: format!("old_str not found in {path}"),
                });
            }
            1 => {}
            n => {
                return Ok(EditResult {
                    success: false,
                    file_path: rel_path.clone(),
                    matched_str: old_str.to_string(),
                    new_str: new_str.to_string(),
                    replaced_span: None,
                    dry_run,
                    diff: None,
                    message: format!("old_str matches {n} times, must match exactly once"),
                });
            }
        }

        let modified = source.replacen(old_str, new_str, 1);

        let diff = self
            .commit_or_preview_edit(
                &rel_path,
                &file,
                &source_identity,
                &source,
                &modified,
                dry_run,
            )
            .await?;

        Ok(EditResult {
            success: true,
            file_path: rel_path,
            matched_str: old_str.to_string(),
            new_str: new_str.to_string(),
            replaced_span: None,
            dry_run,
            diff,
            message: edit_success_message(dry_run, "replacement successful"),
        })
    }

    /// Applies multiple string replacements atomically.
    /// Fails if any `old_str` doesn't match exactly once.
    pub(crate) async fn multi_str_replace(
        &self,
        path: &str,
        replacements: &[(&str, &str)],
        dry_run: bool,
    ) -> Result<MultiEditResult> {
        let rel_path = self
            .resolve_path(path)
            .ok_or_else(|| TraceDecayError::Config {
                message: "path is not within the project".to_string(),
            })?;

        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(path)?;

        // Resolve every replacement against the ORIGINAL source. Each `old`
        // must match exactly once, and no two matched ranges may overlap.
        // Splicing from the original (instead of applying `replacen`
        // sequentially against progressively-edited text) guarantees a later
        // `old` can never match text an earlier replacement introduced, and no
        // match can land at a shifted offset.
        let mut spans: Vec<(usize, usize, &str, &str)> = Vec::with_capacity(replacements.len());
        for (old, new) in replacements {
            let mut hits = source.match_indices(old);
            let Some((start, matched)) = hits.next() else {
                return Ok(MultiEditResult {
                    success: false,
                    file_path: rel_path.clone(),
                    applied_count: 0,
                    dry_run,
                    diff: None,
                    message: format!(
                        "replacement '{}' matches 0 times, must match exactly once",
                        crate::text::utf8_prefix_at_or_before(old, 20)
                    ),
                });
            };
            if hits.next().is_some() {
                let count = source.matches(old).count();
                return Ok(MultiEditResult {
                    success: false,
                    file_path: rel_path.clone(),
                    applied_count: 0,
                    dry_run,
                    diff: None,
                    message: format!(
                        "replacement '{}' matches {} times, must match exactly once",
                        crate::text::utf8_prefix_at_or_before(old, 20),
                        count
                    ),
                });
            }
            spans.push((start, start + matched.len(), old, new));
        }

        // Order by match start so we can both detect overlaps and splice in one
        // left-to-right pass. Touching ranges are fine; only true overlaps (a
        // later match starting inside an earlier one) are rejected.
        spans.sort_by_key(|&(start, _, _, _)| start);
        for window in spans.windows(2) {
            let (_, prev_end, prev_old, _) = window[0];
            let (next_start, _, next_old, _) = window[1];
            if next_start < prev_end {
                return Ok(MultiEditResult {
                    success: false,
                    file_path: rel_path.clone(),
                    applied_count: 0,
                    dry_run,
                    diff: None,
                    message: format!(
                        "replacements '{}' and '{}' target overlapping ranges; apply them separately",
                        crate::text::utf8_prefix_at_or_before(prev_old, 20),
                        crate::text::utf8_prefix_at_or_before(next_old, 20)
                    ),
                });
            }
        }

        let mut modified = String::with_capacity(source.len());
        let mut cursor = 0usize;
        for &(start, end, _, new) in &spans {
            modified.push_str(&source[cursor..start]);
            modified.push_str(new);
            cursor = end;
        }
        modified.push_str(&source[cursor..]);

        let diff = self
            .commit_or_preview_edit(
                &rel_path,
                &file,
                &source_identity,
                &source,
                &modified,
                dry_run,
            )
            .await?;

        Ok(MultiEditResult {
            success: true,
            file_path: rel_path,
            applied_count: replacements.len(),
            dry_run,
            diff,
            message: edit_success_message(
                dry_run,
                &format!("applied {} replacements", replacements.len()),
            ),
        })
    }

    /// Inserts content before or after a unique anchor.
    /// Anchor can be a string or 1-indexed line number.
    pub(crate) async fn insert_at(
        &self,
        path: &str,
        anchor: &str,
        content: &str,
        before: bool,
        dry_run: bool,
    ) -> Result<InsertResult> {
        let rel_path = self
            .resolve_path(path)
            .ok_or_else(|| TraceDecayError::Config {
                message: "path is not within the project".to_string(),
            })?;

        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(path)?;

        let lines: Vec<&str> = source.lines().collect();

        let anchor_line = if anchor.chars().all(|c| c.is_ascii_digit()) {
            let line_num: usize = anchor.parse().map_err(|_| TraceDecayError::Config {
                message: format!("invalid line number: {anchor}"),
            })?;
            if line_num == 0 || line_num > lines.len() {
                return Ok(InsertResult {
                    success: false,
                    file_path: rel_path.clone(),
                    anchor_line: line_num as u32,
                    content: content.to_string(),
                    before,
                    dry_run,
                    diff: None,
                    message: format!(
                        "line number {line_num} out of range (file has {} lines)",
                        lines.len()
                    ),
                });
            }
            line_num - 1
        } else {
            let anchor_prefix = crate::text::utf8_prefix_at_or_before(anchor, 100);
            let matching_lines: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.contains(anchor_prefix))
                .map(|(i, _)| i)
                .collect();

            if matching_lines.is_empty() {
                return Ok(InsertResult {
                    success: false,
                    file_path: rel_path.clone(),
                    anchor_line: 0,
                    content: content.to_string(),
                    before,
                    dry_run,
                    diff: None,
                    message: format!("anchor '{anchor}' not found"),
                });
            }
            if matching_lines.len() > 1 {
                return Ok(InsertResult {
                    success: false,
                    file_path: rel_path.clone(),
                    anchor_line: matching_lines.len() as u32,
                    content: content.to_string(),
                    before,
                    dry_run,
                    diff: None,
                    message: format!(
                        "anchor '{anchor}' matches {} lines, must match exactly one",
                        matching_lines.len()
                    ),
                });
            }
            matching_lines[0]
        };

        let insert_idx = if before { anchor_line } else { anchor_line + 1 };
        let mut new_lines: Vec<&str> = lines[..insert_idx].to_vec();
        new_lines.push(content);
        new_lines.extend_from_slice(&lines[insert_idx..]);
        let mut modified = new_lines.join("\n");
        if source.ends_with('\n') {
            modified.push('\n');
        }

        let diff = self
            .commit_or_preview_edit(
                &rel_path,
                &file,
                &source_identity,
                &source,
                &modified,
                dry_run,
            )
            .await?;

        Ok(InsertResult {
            success: true,
            file_path: rel_path,
            anchor_line: (anchor_line + 1) as u32,
            content: content.to_string(),
            before,
            dry_run,
            diff,
            message: edit_success_message(
                dry_run,
                &format!("inserted at line {}", anchor_line + 1),
            ),
        })
    }

    /// Replaces the full source of a named symbol (function, method, struct,
    /// etc.) with `new_source`. Resolves the symbol via exact qualified-name
    /// match — if the name is ambiguous, callable definitions win; if still
    /// ambiguous after that filter, the edit is refused so we don't clobber
    /// the wrong site.
    pub(crate) async fn replace_symbol(
        &self,
        symbol: &str,
        new_source: &str,
        dry_run: bool,
    ) -> Result<EditResult> {
        let target = resolve_symbol_for_edit(self, symbol).await?;
        let rel_path = target.file_path.clone();
        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(&rel_path)?;
        let lines: Vec<&str> = source.lines().collect();
        // Honor the leading doc-comment / attribute block adaptively. The
        // extractor only sets `attrs_start_line` below `start_line` for an
        // item that actually has such a block. When `new_source` carries its
        // own leading docs/attrs, the whole span (docs included) is swapped so
        // documentation is never duplicated; when it does not, the existing
        // block is preserved above the replacement so replacing a symbol's
        // body never silently deletes its documentation.
        let has_leading_block = (target.attrs_start_line as usize) < target.start_line as usize;
        let replacement_brings_block = leading_doc_or_attr(new_source);
        let start = if has_leading_block && replacement_brings_block {
            target.attrs_start_line as usize
        } else {
            target.start_line as usize
        };
        let end_inclusive = (target.end_line as usize).min(lines.len().saturating_sub(1));
        if start >= lines.len() || start > end_inclusive {
            return Ok(EditResult {
                success: false,
                file_path: rel_path,
                matched_str: symbol.to_string(),
                new_str: String::new(),
                replaced_span: None,
                dry_run,
                diff: None,
                message: format!(
                    "symbol range [{}..={}] out of bounds for {}-line file",
                    start,
                    target.end_line,
                    lines.len()
                ),
            });
        }
        let replaced_span = lines[start..=end_inclusive].join("\n");
        let trailing_newline = source.ends_with('\n');
        let mut rebuilt: Vec<String> = Vec::with_capacity(lines.len());
        rebuilt.extend(lines[..start].iter().map(|s| (*s).to_string()));
        rebuilt.push(new_source.trim_end_matches('\n').to_string());
        rebuilt.extend(lines[end_inclusive + 1..].iter().map(|s| (*s).to_string()));
        let mut modified = rebuilt.join("\n");
        if trailing_newline {
            modified.push('\n');
        }
        let diff = self
            .commit_or_preview_edit(
                &rel_path,
                &file,
                &source_identity,
                &source,
                &modified,
                dry_run,
            )
            .await?;
        // If the old span carried leading docs/attrs but the replacement text
        // does not appear to, surface a note so the caller can recover them
        // from `replaced_span` rather than silently losing documentation.
        let base = format!(
            "replaced {}:{}-{}",
            target.file_path,
            start + 1,
            target.end_line + 1
        );
        let mut message = edit_success_message(dry_run, &base);
        if has_leading_block && !replacement_brings_block {
            message.push_str(
                "; note: the item's leading docs/attrs were preserved above the \
                 replacement — include a leading doc/attr block in new_source to \
                 replace them",
            );
        }
        Ok(EditResult {
            success: true,
            file_path: rel_path,
            matched_str: format!("{} ({})", target.name, target.kind.as_str()),
            new_str: new_source.to_string(),
            replaced_span: Some(replaced_span),
            dry_run,
            diff,
            message,
        })
    }

    /// Inserts `content` immediately before or after a named symbol. `position`
    /// is one of `"before"` or `"after"`. Uses the same resolution logic as
    /// `replace_symbol`.
    pub(crate) async fn insert_at_symbol(
        &self,
        symbol: &str,
        content: &str,
        position: &str,
        dry_run: bool,
    ) -> Result<InsertResult> {
        let before = match position {
            "before" => true,
            "after" => false,
            other => {
                return Err(TraceDecayError::Config {
                    message: format!("position must be \"before\" or \"after\", got {other:?}"),
                });
            }
        };
        let target = resolve_symbol_for_edit(self, symbol).await?;
        let rel_path = target.file_path.clone();
        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(&rel_path)?;
        let lines: Vec<&str> = source.lines().collect();
        // `before` inserts above the item's leading doc-comment / attribute
        // block (when the extractor recorded one) so new content lands above the
        // docs rather than splitting them from their item; `after` is unaffected.
        let anchor_line = if before {
            // Anchor above the item's leading doc-comment / attribute block so
            // "before" never splits docs from the item they document. For items
            // with no leading block, attrs_start_line == start_line and this is
            // the item line itself. The min() guards against inconsistent rows.
            target.attrs_start_line.min(target.start_line) as usize
        } else {
            (target.end_line as usize).saturating_add(1)
        };
        if anchor_line > lines.len() {
            return Ok(InsertResult {
                success: false,
                file_path: rel_path,
                anchor_line: anchor_line as u32,
                content: content.to_string(),
                before,
                dry_run,
                diff: None,
                message: format!("anchor line {anchor_line} past EOF ({})", lines.len()),
            });
        }
        let trailing_newline = source.ends_with('\n');
        let mut rebuilt: Vec<String> = Vec::with_capacity(lines.len() + 1);
        rebuilt.extend(lines[..anchor_line].iter().map(|s| (*s).to_string()));
        rebuilt.push(content.trim_end_matches('\n').to_string());
        rebuilt.extend(lines[anchor_line..].iter().map(|s| (*s).to_string()));
        let mut modified = rebuilt.join("\n");
        if trailing_newline {
            modified.push('\n');
        }
        let diff = self
            .commit_or_preview_edit(
                &rel_path,
                &file,
                &source_identity,
                &source,
                &modified,
                dry_run,
            )
            .await?;
        Ok(InsertResult {
            success: true,
            file_path: rel_path,
            anchor_line: (anchor_line + 1) as u32,
            content: content.to_string(),
            before,
            dry_run,
            diff,
            message: edit_success_message(
                dry_run,
                &format!(
                    "inserted {} {} ({}) at line {}",
                    position,
                    target.name,
                    target.kind.as_str(),
                    anchor_line + 1
                ),
            ),
        })
    }

    /// Performs structural rewrite using ast-grep CLI.
    pub(crate) async fn ast_grep_rewrite(
        &self,
        path: &str,
        pattern: &str,
        rewrite: &str,
        dry_run: bool,
    ) -> Result<AstGrepResult> {
        let rel_path = self
            .resolve_path(path)
            .ok_or_else(|| TraceDecayError::Config {
                message: "path is not within the project".to_string(),
            })?;
        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(path)?;

        let check_output = crate::external_tools::ast_grep_command()
            .args(["--version"])
            .output();

        if check_output.is_err() {
            if can_use_literal_rewrite_fallback(pattern) {
                if !source.contains(pattern) {
                    return Ok(AstGrepResult {
                        success: false,
                        file_path: rel_path.clone(),
                        pattern: pattern.to_string(),
                        rewrite: rewrite.to_string(),
                        dry_run,
                        diff: None,
                        message: "pattern not found (built-in literal fallback)".to_string(),
                    });
                }
                let modified = source.replace(pattern, rewrite);
                let diff = self
                    .commit_or_preview_edit(
                        &rel_path,
                        &file,
                        &source_identity,
                        &source,
                        &modified,
                        dry_run,
                    )
                    .await?;
                return Ok(AstGrepResult {
                    success: true,
                    file_path: rel_path,
                    pattern: pattern.to_string(),
                    rewrite: rewrite.to_string(),
                    dry_run,
                    diff,
                    message: edit_success_message(
                        dry_run,
                        "literal rewrite completed using built-in fallback",
                    ),
                });
            }
            return Ok(AstGrepResult {
                success: false,
                file_path: rel_path.clone(),
                pattern: pattern.to_string(),
                rewrite: rewrite.to_string(),
                dry_run,
                diff: None,
                message: "ast-grep is not installed and this pattern needs SGPattern matching. Simple literal rewrites are handled by the built-in fallback.".to_string(),
            });
        }

        // Always ask ast-grep for its read-only structured replacement plan.
        // Reconstructing the exact post-edit bytes here keeps dry-run capture
        // and real application behind the same write authority.
        let suffix = Path::new(&rel_path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map_or_else(String::new, |extension| format!(".{extension}"));
        let mut snapshot = tempfile::Builder::new()
            .prefix("tracedecay-source-edit-")
            .suffix(&suffix)
            .tempfile()
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to stage source edit analysis snapshot: {error}"),
            })?;
        snapshot
            .write_all(source.as_bytes())
            .and_then(|()| snapshot.flush())
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to write source edit analysis snapshot: {error}"),
            })?;
        let snapshot_path_arg = snapshot.path().to_string_lossy();
        let mut ast_grep_args: Vec<&str> =
            vec!["run", "-p", pattern, "-r", rewrite, "--json=compact"];
        ast_grep_args.push(snapshot_path_arg.as_ref());
        let output = crate::external_tools::ast_grep_command()
            .args(&ast_grep_args)
            .output()
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to run ast-grep: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr_trim = stderr.trim();
            let stdout_trim = stdout.trim();
            let exit = output
                .status
                .code()
                .map_or_else(|| "killed by signal".to_string(), |c| c.to_string());
            let message = if !stderr_trim.is_empty() {
                format!("ast-grep failed (exit {exit}): {stderr_trim}")
            } else if !stdout_trim.is_empty() {
                format!("ast-grep failed (exit {exit}). stdout: {stdout_trim}")
            } else {
                format!(
                    "ast-grep failed (exit {exit}) with no output. Likely causes: \
                     pattern matched 0 nodes, language not inferred from file extension \
                     (e.g. .txt has no parser), or invalid pattern syntax. \
                     File: {rel_path}, pattern: {pattern:?}"
                )
            };
            return Ok(AstGrepResult {
                success: false,
                file_path: rel_path.clone(),
                pattern: pattern.to_string(),
                rewrite: rewrite.to_string(),
                dry_run,
                diff: None,
                message,
            });
        }

        let modified = reconstruct_ast_grep_rewrite(&source, &output.stdout)?;
        let diff = self
            .commit_or_preview_edit(
                &rel_path,
                &file,
                &source_identity,
                &source,
                &modified,
                dry_run,
            )
            .await?;

        Ok(AstGrepResult {
            success: true,
            file_path: rel_path,
            pattern: pattern.to_string(),
            rewrite: rewrite.to_string(),
            dry_run,
            diff,
            message: edit_success_message(dry_run, "ast-grep rewrite completed"),
        })
    }
}

#[derive(serde::Deserialize)]
struct AstGrepJsonReplacement {
    text: String,
    replacement: String,
    #[serde(rename = "replacementOffsets")]
    replacement_offsets: AstGrepJsonOffsets,
}

#[derive(serde::Deserialize)]
struct AstGrepJsonOffsets {
    start: usize,
    end: usize,
}

fn reconstruct_ast_grep_rewrite(source: &str, output: &[u8]) -> Result<String> {
    let mut replacements: Vec<AstGrepJsonReplacement> = if output.is_empty() {
        Vec::new()
    } else {
        serde_json::from_slice(output).map_err(|error| TraceDecayError::Config {
            message: format!("ast-grep returned invalid replacement JSON: {error}"),
        })?
    };
    replacements.sort_by_key(|candidate| {
        (
            candidate.replacement_offsets.start,
            candidate.replacement_offsets.end,
        )
    });

    let mut modified = String::with_capacity(source.len());
    let mut cursor = 0;
    for candidate in replacements {
        let start = candidate.replacement_offsets.start;
        let end = candidate.replacement_offsets.end;
        if start < cursor || end < start {
            return Err(TraceDecayError::Config {
                message: "ast-grep returned overlapping or reversed replacement offsets"
                    .to_string(),
            });
        }
        let Some(matched) = source.get(start..end) else {
            return Err(TraceDecayError::Config {
                message: "ast-grep returned replacement offsets outside UTF-8 source boundaries"
                    .to_string(),
            });
        };
        if matched != candidate.text {
            return Err(TraceDecayError::Config {
                message: "ast-grep replacement offsets did not match the source bytes".to_string(),
            });
        }
        modified.push_str(&source[cursor..start]);
        modified.push_str(&candidate.replacement);
        cursor = end;
    }
    modified.push_str(&source[cursor..]);
    Ok(modified)
}

/// Cheap heuristic: does `source`'s first non-blank line look like a leading
/// doc-comment (`//`, `///`, `//!`), block comment (`/*`), or attribute
/// (`#[`, `#!`)? Used only to decide whether a `replace_symbol` note should
/// warn that the replacement text may have dropped the item's docs/attrs.
fn leading_doc_or_attr(source: &str) -> bool {
    source
        .lines()
        .map(str::trim_start)
        .find(|line| !line.is_empty())
        .is_some_and(|line| {
            line.starts_with("//")
                || line.starts_with("/*")
                || line.starts_with("#[")
                || line.starts_with("#!")
        })
}

fn can_use_literal_rewrite_fallback(pattern: &str) -> bool {
    let trimmed = pattern.trim();
    !trimmed.is_empty()
        && trimmed == pattern
        && !pattern.contains('$')
        && !pattern.contains('\n')
        && !pattern.contains('\r')
}

/// Unchanged context lines shown on each side of the changed region in a
/// dry-run preview diff.
pub(super) const PREVIEW_DIFF_CONTEXT: usize = 3;

/// Hard cap on the number of lines emitted in a dry-run preview diff. Keeps the
/// preview bounded even when an edit rewrites a large span; the remainder is
/// noted as truncated.
pub(super) const MAX_PREVIEW_DIFF_LINES: usize = 200;

/// Success-path message wrapper: on a real edit returns `base` verbatim; on a
/// dry run wraps it to make clear that nothing was written and only a preview
/// was produced.
pub(super) fn edit_success_message(dry_run: bool, base: &str) -> String {
    if dry_run {
        format!("dry run — nothing written; preview only ({base})")
    } else {
        base.to_string()
    }
}

/// Builds a bounded, unified-style diff of the single changed region between
/// `original` and `modified`. The two texts are compared line-by-line: the
/// common leading and trailing lines are trimmed and only the differing middle
/// band — plus `context` unchanged lines on each side — is rendered, capped at
/// `max_lines` (excess is noted as truncated). This is a cheap single-hunk
/// preview for a localized edit, not a minimal multi-hunk LCS diff; a widely
/// scattered set of changes collapses into one hunk spanning them.
pub(super) fn bounded_region_diff(
    original: &str,
    modified: &str,
    context: usize,
    max_lines: usize,
) -> String {
    if original == modified {
        return "(no changes)".to_string();
    }
    let old: Vec<&str> = original.lines().collect();
    let new: Vec<&str> = modified.lines().collect();

    // Longest common line prefix.
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    // Longest common line suffix that does not overlap the prefix.
    let mut suffix = 0;
    while suffix < old.len().saturating_sub(prefix)
        && suffix < new.len().saturating_sub(prefix)
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let old_change_end = old.len() - suffix; // exclusive
    let new_change_end = new.len() - suffix; // exclusive
    let ctx_start = prefix.saturating_sub(context);
    let old_ctx_end = (old_change_end + context).min(old.len());
    let new_ctx_end = (new_change_end + context).min(new.len());

    let mut out: Vec<String> = Vec::new();
    out.push(format!(
        "@@ -{},{} +{},{} @@",
        ctx_start + 1,
        old_ctx_end - ctx_start,
        ctx_start + 1,
        new_ctx_end - ctx_start
    ));
    for line in &old[ctx_start..prefix] {
        out.push(format!(" {line}"));
    }
    for line in &old[prefix..old_change_end] {
        out.push(format!("-{line}"));
    }
    for line in &new[prefix..new_change_end] {
        out.push(format!("+{line}"));
    }
    for line in &new[new_change_end..new_ctx_end] {
        out.push(format!(" {line}"));
    }

    if out.len() > max_lines {
        let omitted = out.len() - max_lines;
        out.truncate(max_lines);
        out.push(format!("... diff truncated ({omitted} more line(s))"));
    }
    out.join("\n")
}

/// Resolves a symbol name to a single node suitable for symbol-aware editing.
///
/// Exact-qualified-name match wins. Bare-name ambiguity may narrow to callable
/// kinds (function/method/etc.); remaining ambiguity — bare or qualified —
/// narrows to declaration kinds, because a type's inherent `impl` blocks share
/// its qualified name but "edit `Foo`" means the `Foo` declaration (impl
/// blocks are separate spans the caller edits explicitly). Anything still
/// ambiguous refuses the edit — silently picking the wrong site is worse than
/// asking the caller to disambiguate.
pub(super) async fn resolve_symbol_for_edit(cg: &TraceDecay, symbol: &str) -> Result<Node> {
    let nodes = cg.get_nodes_by_qualified_name(symbol).await?;
    narrow_symbol_for_edit(symbol, nodes)
}

/// Pure narrowing behind [`resolve_symbol_for_edit`]; split out so the
/// ambiguity rules are unit-testable without a graph database.
fn narrow_symbol_for_edit(symbol: &str, nodes: Vec<Node>) -> Result<Node> {
    let mut iter = nodes.into_iter();
    let Some(first) = iter.next() else {
        return Err(TraceDecayError::Config {
            message: format!("symbol '{symbol}' not found"),
        });
    };
    let rest: Vec<Node> = iter.collect();
    if rest.is_empty() {
        return Ok(first);
    }
    let total = rest.len() + 1;
    let all: Vec<Node> = std::iter::once(first).chain(rest).collect();
    if !symbol.contains("::") {
        let mut callables: Vec<Node> = all
            .iter()
            .filter(|node| is_callable_edit_kind(&node.kind))
            .cloned()
            .collect();
        if callables.len() == 1 {
            return Ok(callables.remove(0));
        }
    }
    let mut declarations: Vec<Node> = all
        .into_iter()
        .filter(|node| !matches!(node.kind, NodeKind::Impl))
        .collect();
    if declarations.len() == 1 {
        return Ok(declarations.remove(0));
    }
    if symbol.contains("::") {
        return Err(TraceDecayError::Config {
            message: format!(
                "symbol '{symbol}' is ambiguous ({total} matches); pass an exact stored qualified name"
            ),
        });
    }
    Err(TraceDecayError::Config {
        message: format!(
            "symbol '{symbol}' is ambiguous ({total} matches); pass a fully qualified name"
        ),
    })
}

/// Kinds that win bare-name ambiguity: the callable definitions a caller
/// almost always means when naming `foo` without qualification.
fn is_callable_edit_kind(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::StructMethod
            | NodeKind::Constructor
            | NodeKind::AbstractMethod
            | NodeKind::ArrowFunction
            | NodeKind::Procedure
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[cfg(unix)]
    use super::read_source_edit_candidate;
    use super::{
        SourceEditFileAuthority, capture_planned_source_edit, capture_source_edit_plan,
        leading_doc_or_attr, narrow_symbol_for_edit, publish_planned_source_edit,
        reconstruct_ast_grep_rewrite, rollback_api_migration_files,
    };
    use crate::types::{Node, NodeKind, Visibility};
    use tempfile::tempdir;

    fn node(kind: NodeKind, name: &str) -> Node {
        Node {
            id: format!("{kind:?}:{name}"),
            kind,
            name: name.to_string(),
            qualified_name: format!("src/a.rs::{name}"),
            file_path: "src/a.rs".to_string(),
            start_line: 1,
            attrs_start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 1,
            signature: None,
            docstring: None,
            visibility: Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: 0,
            parent_id: None,
        }
    }

    #[tokio::test]
    async fn source_edit_plan_capture_retains_exact_pre_and_post_bytes() {
        let (_, files) = capture_source_edit_plan(async {
            capture_planned_source_edit("src/lib.rs", Some("before\n"), Some("after\n"));
        })
        .await;

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "src/lib.rs");
        assert_eq!(files[0].expected.as_deref(), Some("before\n"));
        assert_eq!(files[0].intended.as_deref(), Some("after\n"));
    }

    #[test]
    fn api_migration_rollback_restores_every_published_preimage() {
        let root = tempdir().expect("temporary project");
        std::fs::create_dir_all(root.path().join("src")).expect("create source directory");
        let candidates = [
            ("src/a.rs", "pub fn old_a() {}\n", "pub fn new_a() {}\n"),
            ("src/b.rs", "pub fn old_b() {}\n", "pub fn new_b() {}\n"),
        ]
        .into_iter()
        .map(|(path, expected, intended)| {
            std::fs::write(root.path().join(path), expected).expect("seed source");
            tracedecay_application::ApiMigrationFilePlanV1 {
                path: path.to_owned(),
                expected_digest: tracedecay_application::api_migration_file_digest(expected)
                    .expect("expected digest"),
                predicted_digest: tracedecay_application::api_migration_file_digest(intended)
                    .expect("predicted digest"),
                expected_content: expected.to_owned(),
                intended_content: intended.to_owned(),
            }
        })
        .collect::<Vec<_>>();

        for candidate in &candidates {
            let file = SourceEditFileAuthority::open(root.path(), Path::new(&candidate.path))
                .expect("open source authority");
            let identity = file.current_identity().expect("read identity");
            file.publish(
                &candidate.path,
                Some(&candidate.expected_content),
                identity.as_ref(),
                &candidate.intended_content,
                || {},
            )
            .expect("publish candidate");
        }

        let published = candidates.iter().collect::<Vec<_>>();
        rollback_api_migration_files(root.path(), &published).expect("rollback migration");
        for candidate in &candidates {
            assert_eq!(
                std::fs::read_to_string(root.path().join(&candidate.path)).expect("read restored"),
                candidate.expected_content
            );
        }
    }

    #[test]
    fn ast_grep_reconstruction_uses_exact_validated_offsets() {
        let output =
            br#"[{"text":"old","replacement":"new","replacementOffsets":{"start":3,"end":6}}]"#;
        assert_eq!(
            reconstruct_ast_grep_rewrite("fn old() {}\n", output).unwrap(),
            "fn new() {}\n"
        );
    }

    #[test]
    fn ast_grep_reconstruction_rejects_mismatched_source() {
        let output =
            br#"[{"text":"not-old","replacement":"new","replacementOffsets":{"start":3,"end":6}}]"#;
        assert!(reconstruct_ast_grep_rewrite("fn old() {}\n", output).is_err());
    }

    #[test]
    fn atomic_publication_rejects_content_drift() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("lib.rs");
        std::fs::write(&path, "changed\n").unwrap();

        assert!(
            publish_planned_source_edit(
                directory.path(),
                "lib.rs",
                Some("previewed\n"),
                "intended\n"
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "changed\n");
    }

    #[test]
    fn atomic_publication_rejects_same_content_inode_swap() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("lib.rs");
        let replacement = directory.path().join("replacement.rs");
        std::fs::write(&path, "previewed\n").unwrap();
        std::fs::write(&replacement, "previewed\n").unwrap();
        let file = SourceEditFileAuthority::open(directory.path(), Path::new("lib.rs")).unwrap();
        let (_, identity) = file.read_to_string("lib.rs").unwrap();

        assert!(
            file.publish(
                "lib.rs",
                Some("previewed\n"),
                Some(&identity),
                "intended\n",
                || std::fs::rename(&replacement, &path).unwrap(),
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "previewed\n");
    }

    #[test]
    fn atomic_publication_rejects_parent_directory_rebinding() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("src");
        let moved = directory.path().join("moved");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("lib.rs"), "previewed\n").unwrap();
        let file =
            SourceEditFileAuthority::open(directory.path(), Path::new("src/lib.rs")).unwrap();
        let (_, identity) = file.read_to_string("src/lib.rs").unwrap();

        assert!(
            file.publish(
                "src/lib.rs",
                Some("previewed\n"),
                Some(&identity),
                "intended\n",
                || {
                    std::fs::rename(&source, &moved).unwrap();
                    std::fs::create_dir(&source).unwrap();
                    std::fs::write(source.join("lib.rs"), "replacement\n").unwrap();
                },
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(moved.join("lib.rs")).unwrap(),
            "previewed\n"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("lib.rs")).unwrap(),
            "replacement\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_read_rejects_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("lib.rs"), "outside\n").unwrap();
        symlink(outside.path(), directory.path().join("src")).unwrap();

        assert!(read_source_edit_candidate(directory.path(), Path::new("src/lib.rs")).is_err());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("lib.rs")).unwrap(),
            "outside\n"
        );
    }

    #[test]
    fn leading_doc_or_attr_detects_doc_comment() {
        assert!(leading_doc_or_attr("/// docs\nfn f() {}"));
        assert!(leading_doc_or_attr("//! module doc\nfn f() {}"));
        assert!(leading_doc_or_attr("// plain\nfn f() {}"));
    }

    #[test]
    fn leading_doc_or_attr_detects_attribute_and_block_comment() {
        assert!(leading_doc_or_attr("#[inline]\nfn f() {}"));
        assert!(leading_doc_or_attr("#![allow(dead_code)]"));
        assert!(leading_doc_or_attr("/* block */\nfn f() {}"));
    }

    #[test]
    fn leading_doc_or_attr_skips_leading_blank_lines() {
        assert!(leading_doc_or_attr("\n\n   /// docs\nfn f() {}"));
        assert!(!leading_doc_or_attr("\n\nfn f() {}"));
    }

    #[test]
    fn leading_doc_or_attr_false_for_bare_code() {
        assert!(!leading_doc_or_attr("fn f() {}"));
        assert!(!leading_doc_or_attr(""));
        assert!(!leading_doc_or_attr("pub struct S;"));
    }

    use super::{bounded_region_diff, edit_success_message};

    #[test]
    fn bounded_region_diff_reports_no_changes_when_identical() {
        assert_eq!(
            bounded_region_diff("a\nb\n", "a\nb\n", 3, 200),
            "(no changes)"
        );
    }

    #[test]
    fn bounded_region_diff_shows_changed_line_with_context() {
        let original = "one\ntwo\nthree\nfour\nfive\n";
        let modified = "one\ntwo\nTHREE\nfour\nfive\n";
        let diff = bounded_region_diff(original, modified, 1, 200);
        assert!(diff.contains("-three"), "diff should mark removal: {diff}");
        assert!(diff.contains("+THREE"), "diff should mark addition: {diff}");
        // One line of context on each side, but not the far-away lines.
        assert!(
            diff.contains(" two"),
            "diff should include leading context: {diff}"
        );
        assert!(
            diff.contains(" four"),
            "diff should include trailing context: {diff}"
        );
        assert!(
            !diff.contains("one"),
            "distant lines should be trimmed: {diff}"
        );
        assert!(
            diff.starts_with("@@"),
            "diff should carry a hunk header: {diff}"
        );
    }

    #[test]
    fn bounded_region_diff_handles_pure_insertion() {
        let diff = bounded_region_diff("a\nb\n", "a\nNEW\nb\n", 3, 200);
        assert!(diff.contains("+NEW"), "insertion should appear: {diff}");
        assert!(
            !diff.lines().any(|line| line.starts_with('-')),
            "pure insertion has no removals: {diff}"
        );
    }

    #[test]
    fn bounded_region_diff_truncates_past_the_cap() {
        let original = "keep\n";
        let modified: String = std::iter::once("keep")
            .chain((0..500).map(|_| "x"))
            .collect::<Vec<_>>()
            .join("\n");
        let diff = bounded_region_diff(original, &modified, 3, 50);
        assert!(
            diff.contains("diff truncated"),
            "large diff should truncate: {diff}"
        );
    }

    #[test]
    fn edit_success_message_marks_dry_runs() {
        assert_eq!(edit_success_message(false, "done"), "done");
        let dry = edit_success_message(true, "done");
        assert!(
            dry.contains("dry run"),
            "dry-run message should say so: {dry}"
        );
        assert!(
            dry.contains("done"),
            "dry-run message should keep the base: {dry}"
        );
    }

    #[test]
    fn narrow_symbol_prefers_declaration_over_impl_blocks() {
        let resolved = narrow_symbol_for_edit(
            "src/a.rs::Widget",
            vec![
                node(NodeKind::Struct, "Widget"),
                node(NodeKind::Impl, "Widget"),
                node(NodeKind::Impl, "Widget"),
            ],
        )
        .expect("declaration should win over same-named impl blocks");
        assert_eq!(resolved.kind, NodeKind::Struct);
    }

    #[test]
    fn narrow_symbol_prefers_declaration_for_bare_names() {
        let resolved = narrow_symbol_for_edit(
            "Widget",
            vec![
                node(NodeKind::Impl, "Widget"),
                node(NodeKind::Enum, "Widget"),
            ],
        )
        .expect("bare name should narrow to the declaration");
        assert_eq!(resolved.kind, NodeKind::Enum);
    }

    #[test]
    fn narrow_symbol_keeps_callable_precedence_for_bare_names() {
        let resolved = narrow_symbol_for_edit(
            "run",
            vec![
                node(NodeKind::Module, "run"),
                node(NodeKind::Function, "run"),
            ],
        )
        .expect("bare name should keep the historical callable-wins rule");
        assert_eq!(resolved.kind, NodeKind::Function);
    }

    #[test]
    fn narrow_symbol_still_refuses_multiple_declarations() {
        let result = narrow_symbol_for_edit(
            "src/a.rs::Widget",
            vec![
                node(NodeKind::Struct, "Widget"),
                node(NodeKind::Struct, "Widget"),
            ],
        );
        assert!(result.is_err(), "two declarations must stay ambiguous");
    }

    #[test]
    fn narrow_symbol_still_refuses_impl_only_matches() {
        let result = narrow_symbol_for_edit(
            "src/a.rs::Widget",
            vec![
                node(NodeKind::Impl, "Widget"),
                node(NodeKind::Impl, "Widget"),
            ],
        );
        assert!(result.is_err(), "impl blocks alone must stay ambiguous");
    }
}
