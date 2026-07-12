//! Host-loadable materialization of managed skills (Hermes skill-directory
//! analogue).
//!
//! Managed skills live in the `TraceDecay` profile store and are surfaced to
//! prompt-index hosts through a marker block that points at the
//! `tracedecay_skill_view` MCP tool (see [`crate::automation::skill_targets`]).
//! That is discoverable but never *natively loaded*: the host does not treat a
//! managed skill as one of its own skills.
//!
//! This module closes that gap the way Hermes does — by writing each active
//! managed skill as a real, host-loadable `SKILL.md` into the host's own skills
//! directory (`<base>/.claude/skills/<slug>/SKILL.md` for Claude Code, the
//! `.codex` twin for Codex), so the agent loads it like any other skill.
//!
//! Ownership is provenance-scoped. Every materialized file carries
//! `managed-by: tracedecay-automation`, the `skill-id`, and a body
//! `content-hash` in its frontmatter. The reconciler updates or removes *only*
//! files carrying that marker whose recorded hash still matches the file on
//! disk. A user (or the repo's own dev skills under the same directory) that
//! edits a materialized file forks it: the reconciler then leaves it untouched
//! and [`doctor_scope`] reports the drift.
//!
//! Lifecycle:
//! - **activate** (`skills approve` → Active, or auto-enable) → materialize.
//! - **deactivate/archive/disable/remove** → the skill drops out of the active
//!   set and the reconciler removes its materialized file (fork-protected).
//! - **body update** → re-materialize (hash changes, file rewritten).
//! - **`tracedecay update` / install** → reconcile every detected host+scope.
//! - **`tracedecay doctor`** → report missing/forked/orphaned materializations.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::config_error;
pub use crate::automation::managed_skills::managed_skill_root;
use crate::automation::managed_skills::{ManagedSkill, ManagedSkillState};
use crate::automation::skill_frontmatter::{SkillFrontmatterValue, parse_skill_frontmatter};
use crate::errors::Result;

pub use crate::automation::managed_skill_model::MATERIALIZED_SKILL_MANAGED_BY;

const SKILL_FILE: &str = "SKILL.md";
const MATERIALIZATION_MANIFEST_FILE: &str = ".tracedecay-materialization.json";
const MATERIALIZATION_PENDING_FILE: &str = ".tracedecay-materialization.pending.json";

/// A host whose native skills directory can load a materialized `SKILL.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationHost {
    Claude,
    Codex,
}

impl MaterializationHost {
    /// Directory (relative to a scope base) that holds `<slug>/SKILL.md`.
    pub fn skills_subdir(self) -> &'static Path {
        match self {
            Self::Claude => Path::new(".claude/skills"),
            Self::Codex => Path::new(".codex/skills"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// Both hosts, in a stable order.
    pub fn all() -> [MaterializationHost; 2] {
        [Self::Claude, Self::Codex]
    }
}

/// Whether a destination is a project checkout or the user's global home.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationScopeKind {
    Project,
    Global,
}

impl MaterializationScopeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Global => "global",
        }
    }
}

/// One materialization destination: a host skills directory rooted at a base
/// directory (a project checkout, or the user's home for the global scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializationScope {
    pub host: MaterializationHost,
    pub kind: MaterializationScopeKind,
    /// Directory that contains `.claude` / `.codex` (project root or home).
    pub base_dir: PathBuf,
}

impl MaterializationScope {
    pub fn project(host: MaterializationHost, project_root: impl Into<PathBuf>) -> Self {
        Self {
            host,
            kind: MaterializationScopeKind::Project,
            base_dir: project_root.into(),
        }
    }

    pub fn global(host: MaterializationHost, home: impl Into<PathBuf>) -> Self {
        Self {
            host,
            kind: MaterializationScopeKind::Global,
            base_dir: home.into(),
        }
    }

    /// `<base>/.claude/skills` (or the `.codex` twin).
    pub fn skills_dir(&self) -> PathBuf {
        self.base_dir.join(self.host.skills_subdir())
    }

    fn skill_dir(&self, slug: &str) -> PathBuf {
        self.skills_dir().join(slug)
    }

    fn skill_md(&self, slug: &str) -> PathBuf {
        self.skill_dir(slug).join(SKILL_FILE)
    }

    /// Human-readable `host/scope` label for reports and doctor output.
    pub fn describe(&self) -> String {
        format!("{}/{}", self.host.label(), self.kind.label())
    }
}

/// Outcome of materializing one skill into one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializeAction {
    /// The file was created or rewritten to match the active skill.
    Written,
    /// The file already matched the active skill; nothing changed.
    Unchanged,
    /// A file already occupies the slot but is not `TraceDecay`-managed (a user
    /// or repo-local dev skill); left untouched.
    SkippedForeign,
    /// A `TraceDecay`-managed file was edited by the user (fork); left untouched.
    SkippedForked,
}

impl MaterializeAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Written => "written",
            Self::Unchanged => "unchanged",
            Self::SkippedForeign => "skipped_foreign",
            Self::SkippedForked => "skipped_forked",
        }
    }
}

/// Outcome of removing one materialized skill from one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveAction {
    /// The managed file was deleted.
    Removed,
    /// No file was present for the slug.
    Absent,
    /// A file exists but is not `TraceDecay`-managed; left untouched.
    SkippedForeign,
    /// A managed file was user-edited (fork); left untouched.
    SkippedForked,
}

impl RemoveAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::Absent => "absent",
            Self::SkippedForeign => "skipped_foreign",
            Self::SkippedForked => "skipped_forked",
        }
    }
}

/// A single materialize result within a reconcile report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeEntry {
    pub skill_id: String,
    pub path: PathBuf,
    pub action: MaterializeAction,
}

/// A single removal result within a reconcile report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveEntry {
    pub skill_id: String,
    pub path: PathBuf,
    pub action: RemoveAction,
}

/// Result of reconciling one scope against the active managed-skill set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub materialized: Vec<MaterializeEntry>,
    pub removed: Vec<RemoveEntry>,
    /// Per-skill errors encountered while reconciling this scope. One failing
    /// package never aborts materialization or orphan cleanup of the rest.
    pub errors: Vec<String>,
}

impl ReconcileReport {
    pub fn written_count(&self) -> usize {
        self.materialized
            .iter()
            .filter(|entry| entry.action == MaterializeAction::Written)
            .count()
    }

    pub fn removed_count(&self) -> usize {
        self.removed
            .iter()
            .filter(|entry| entry.action == RemoveAction::Removed)
            .count()
    }

    pub fn forked_count(&self) -> usize {
        self.materialized
            .iter()
            .filter(|entry| entry.action == MaterializeAction::SkippedForked)
            .count()
            + self
                .removed
                .iter()
                .filter(|entry| entry.action == RemoveAction::SkippedForked)
                .count()
    }
}

/// A drift finding reported by [`doctor_scope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillDrift {
    /// An active skill has no materialized file in this scope.
    Missing { skill_id: String, path: PathBuf },
    /// A managed file was edited by the user; the reconciler will not clobber
    /// it (the skill is effectively user-forked here).
    Forked { skill_id: String, path: PathBuf },
    /// A foreign file occupies the slot an active skill would materialize to.
    Conflict { skill_id: String, path: PathBuf },
    /// A managed file exists for a skill that is no longer active; a reconcile
    /// would remove it.
    Orphan { skill_id: String, path: PathBuf },
    /// A managed file for a no-longer-active skill that this installation did
    /// not author (committed by another installation, or a legacy manifest with
    /// no recorded author). `tracedecay update` will refuse to remove it, so
    /// doctor must not prescribe update.
    ForeignOrphan { skill_id: String, path: PathBuf },
    /// A non-fatal problem: a per-skill check failed, or two active skill ids
    /// collide on the same host slug. Reported so one bad package never hides
    /// drift for the rest of the scope.
    Warning {
        skill_id: String,
        path: PathBuf,
        message: String,
    },
}

impl SkillDrift {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Missing { .. } => "missing",
            Self::Forked { .. } => "forked",
            Self::Conflict { .. } => "conflict",
            Self::Orphan { .. } => "orphan",
            Self::ForeignOrphan { .. } => "foreign-orphan",
            Self::Warning { .. } => "warning",
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Missing { path, .. }
            | Self::Forked { path, .. }
            | Self::Conflict { path, .. }
            | Self::Orphan { path, .. }
            | Self::ForeignOrphan { path, .. }
            | Self::Warning { path, .. } => path,
        }
    }

    pub fn skill_id(&self) -> &str {
        match self {
            Self::Missing { skill_id, .. }
            | Self::Forked { skill_id, .. }
            | Self::Conflict { skill_id, .. }
            | Self::Orphan { skill_id, .. }
            | Self::ForeignOrphan { skill_id, .. }
            | Self::Warning { skill_id, .. } => skill_id,
        }
    }

    /// The human-readable detail for a [`Self::Warning`], if any.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Warning { message, .. } => Some(message.as_str()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Provenance parsing / fork detection
// ---------------------------------------------------------------------------

/// The provenance a materialized file carries, plus the body markdown as it
/// currently sits on disk (for fork detection).
struct FileProvenance {
    managed_by: Option<String>,
    skill_id: Option<String>,
    content_hash: Option<String>,
    body_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MaterializationManifest {
    managed_by: String,
    skill_id: String,
    package_hash: String,
    /// Stable id of the profile/installation that materialized this package.
    /// Absent on manifests written before this field existed (and on packages
    /// re-derived from disk without a known installation); such packages are
    /// never auto-removed from a *project* scope — they may be another user's
    /// committed files — but doctor still reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    materialized_by: Option<String>,
    files: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PendingMaterialization {
    managed_by: String,
    skill_id: String,
    previous_files: BTreeMap<String, String>,
    remove_files: BTreeMap<String, String>,
    next_manifest: MaterializationManifest,
    artifacts_hex: BTreeMap<String, String>,
}

enum ManifestState {
    Missing,
    Owned(MaterializationManifest),
    Foreign,
}

enum PendingState {
    Missing,
    Owned(Box<PendingMaterialization>),
    Foreign,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArtifactState {
    Missing,
    Clean,
    Forked,
}

impl FileProvenance {
    fn is_managed(&self) -> bool {
        self.managed_by.as_deref() == Some(MATERIALIZED_SKILL_MANAGED_BY)
    }

    /// Legacy (pre-package-hash, PR #362) fork check: the recorded
    /// `content-hash` was the body-only hash, so a file is a fork when the body
    /// on disk no longer hashes to it. Only meaningful for the body-hash domain;
    /// callers first try [`recompute_on_disk_package`] for the package-hash
    /// domain (PR #366+). A managed file missing a content-hash is treated as
    /// forked so we never silently overwrite something we cannot verify.
    fn is_legacy_forked(&self) -> bool {
        match (&self.content_hash, &self.body_hash) {
            (Some(recorded), Some(actual)) => recorded != actual,
            _ => true,
        }
    }
}

/// Reserved package-support paths that are never user support files.
fn is_reserved_support_path(relative: &Path) -> bool {
    use std::path::Component;
    relative.components().any(|component| match component {
        Component::Normal(part) => {
            let part = part.to_string_lossy();
            part == SKILL_FILE
                || part == MATERIALIZATION_MANIFEST_FILE
                || part == MATERIALIZATION_PENDING_FILE
                || part.ends_with(".new")
        }
        _ => true,
    })
}

/// Collects the on-disk support files of a materialized package (every regular
/// file under `dir` except `SKILL.md`, the manifest/pending sidecars, and
/// `*.new` staging), returned sorted by relative path to mirror the ordering
/// [`ManagedSkill::materialized_package_hash`] hashes support files in.
fn collect_on_disk_support_files(dir: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                // Never follow symlinks when recomputing a package from disk.
                continue;
            }
            if file_type.is_dir() {
                walk(base, &path, out)?;
                continue;
            }
            let Ok(relative) = path.strip_prefix(base) else {
                continue;
            };
            if is_reserved_support_path(relative) || safe_support_relative(relative).is_err() {
                continue;
            }
            out.push((relative.to_path_buf(), fs::read(&path)?));
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(out)
}

/// Recomputes the package hash of a manifest-less managed package directly from
/// disk (PR #366+ package-hash domain). Reconstructs the render placeholder by
/// swapping the recorded `content-hash` back to `<package-hash>`, folds in the
/// on-disk support files exactly as [`ManagedSkill::materialized_package_hash`]
/// does, and — when the result matches the recorded hash — returns a re-derived
/// manifest proving the package is pristine (safe to treat as owned). Returns
/// `Ok(None)` when the file is missing, has no content-hash, or has drifted.
fn recompute_on_disk_package(
    dir: &Path,
    provenance: &FileProvenance,
) -> Result<Option<MaterializationManifest>> {
    use sha2::{Digest, Sha256};

    let Some(recorded) = provenance.content_hash.as_deref() else {
        return Ok(None);
    };
    let skill_md_path = artifact_path(dir, SKILL_FILE)?;
    let skill_bytes = match fs::read(&skill_md_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let Ok(contents) = std::str::from_utf8(&skill_bytes) else {
        return Ok(None);
    };
    let needle = format!("content-hash: {recorded}\n");
    if contents.matches(&needle).count() != 1 {
        return Ok(None);
    }
    let reconstructed = contents.replacen(&needle, "content-hash: <package-hash>\n", 1);

    let supports = collect_on_disk_support_files(dir)?;
    let mut hasher = Sha256::new();
    hasher.update(reconstructed.as_bytes());
    let mut files = BTreeMap::new();
    files.insert(SKILL_FILE.to_string(), hash_bytes(&skill_bytes));
    for (relative, bytes) in &supports {
        // Hash the slash-normalized key, not Path display form. On Windows,
        // `strip_prefix` relatives stringify with `\`, while authoring hashes
        // use forward-slash support paths — a mismatch would make every
        // pristine lost-manifest package look forked.
        let key = support_relative_key(relative)?;
        hasher.update(b"\0file:");
        hasher.update(key.as_bytes());
        hasher.update(b"\0");
        hasher.update(bytes);
        files.insert(key, hash_bytes(bytes));
    }
    let recomputed = format!("sha256:{}", hex::encode(hasher.finalize()));
    if recomputed != recorded {
        return Ok(None);
    }
    Ok(Some(MaterializationManifest {
        managed_by: MATERIALIZED_SKILL_MANAGED_BY.to_string(),
        skill_id: provenance.skill_id.clone().unwrap_or_default(),
        package_hash: recorded.to_string(),
        // Unknown authoring installation: a re-derived manifest is never used to
        // authorize cross-profile removal in project scopes.
        materialized_by: None,
        files,
    }))
}

fn frontmatter_scalar<'a>(
    fields: &'a std::collections::BTreeMap<String, SkillFrontmatterValue>,
    key: &str,
) -> Option<&'a str> {
    fields.get(key).and_then(SkillFrontmatterValue::as_scalar)
}

/// Extracts the raw body region after the leading frontmatter block, then
/// strips exactly one leading and one trailing newline to recover the original
/// `body_markdown` we wrote. Returns `None` when the file has no frontmatter.
fn on_disk_body_markdown(contents: &str) -> Option<String> {
    let after_open = contents.strip_prefix("---\n")?;
    let close_at = after_open.find("\n---\n")?;
    let region = &after_open[close_at + "\n---\n".len()..];
    let region = region.strip_prefix('\n').unwrap_or(region);
    let region = region.strip_suffix('\n').unwrap_or(region);
    Some(region.to_string())
}

fn hash_body(body: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex::encode(Sha256::digest(body.as_bytes())))
}

const INSTALLATION_ID_FILE: &str = ".materialization-installation-id";

/// Returns a stable id for the local profile/installation, persisting a random
/// token in the profile root on first use. Stamped into every manifest this
/// installation writes so orphan cleanup in *project* scopes only removes files
/// this installation authored — never another user's committed materialization.
///
/// Best-effort: if the token cannot be read or written (read-only profile), a
/// per-process fallback is returned. A fallback id never matches a persisted
/// manifest id, so cleanup stays conservative (reports, does not delete).
pub fn installation_id(profile_root: &Path) -> String {
    let path = profile_root.join(INSTALLATION_ID_FILE);
    if let Ok(existing) = fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let mut token = [0u8; 16];
    let generated = match getrandom::getrandom(&mut token) {
        Ok(()) => hex::encode(token),
        Err(_) => format!(
            "pid-{}-{}",
            std::process::id(),
            crate::tracedecay::current_timestamp()
        ),
    };
    if fs::create_dir_all(profile_root).is_ok() {
        // Ignore races/read-only failures: a transient id is still safe.
        let _ = fs::write(&path, format!("{generated}\n"));
    }
    // Re-read so a concurrent creator's token wins deterministically.
    match fs::read_to_string(&path) {
        Ok(existing) if !existing.trim().is_empty() => existing.trim().to_string(),
        _ => generated,
    }
}

fn read_file_provenance(path: &Path) -> Result<Option<FileProvenance>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let fields = parse_skill_frontmatter(&contents).ok();
    let (managed_by, skill_id, content_hash) = match &fields {
        Some(fields) => (
            frontmatter_scalar(fields, "managed-by").map(str::to_string),
            frontmatter_scalar(fields, "skill-id").map(str::to_string),
            frontmatter_scalar(fields, "content-hash").map(str::to_string),
        ),
        None => (None, None, None),
    };
    let body_hash = on_disk_body_markdown(&contents).map(|body| hash_body(&body));
    Ok(Some(FileProvenance {
        managed_by,
        skill_id,
        content_hash,
        body_hash,
    }))
}

/// Inter-process lock held for the duration of a single package's
/// materialize/remove transaction. Serializes concurrent `tracedecay update` /
/// `skills approve` / background auto-enable runs (and multiple worktrees
/// sharing a global scope) so the TOCTOU pending-file window cannot interleave
/// two transactions and wedge a package as forked. The lock file lives outside
/// the tracked skills tree (OS temp dir, keyed by the package path) so it never
/// pollutes a repo; flock semantics only need to hold within one machine, which
/// is exactly where the race occurs.
struct PackageLock(fs::File);

impl Drop for PackageLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

fn package_lock_path(package_dir: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let key = hex::encode(Sha256::digest(package_dir.to_string_lossy().as_bytes()));
    std::env::temp_dir().join(format!("tracedecay-materialization-{key}.lock"))
}

fn lock_package(package_dir: &Path) -> Result<PackageLock> {
    use fs2::FileExt;
    let path = package_lock_path(package_dir);
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    crate::storage::retry_transient_file_op(|| file.lock_exclusive())?;
    Ok(PackageLock(file))
}

fn relative_artifact_path(relative: &str) -> Result<&Path> {
    let path = Path::new(relative);
    safe_support_relative(path)?;
    if matches!(
        relative,
        MATERIALIZATION_MANIFEST_FILE | MATERIALIZATION_PENDING_FILE
    ) {
        return Err(config_error(format!(
            "reserved materialized support path '{relative}'"
        )));
    }
    Ok(path)
}

fn ensure_not_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(config_error(format!(
            "refusing materialized skill path through symlink '{}'",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Rejects a symlink at any component the reconciler itself creates — i.e. only
/// the `relative` components under `base`. `base` (the host skills directory and
/// everything above it) may legitimately traverse symlinks: dotfile managers
/// (stow, chezmoi, nix-home-manager) routinely symlink `~/.claude`, `.codex`, or
/// even `/home` itself, and those are normal setups, not private-store roots.
/// We follow them (operate on the resolved target) and guard only the final
/// slug/support components we write into.
fn ensure_no_symlink_components(base: &Path, relative: &Path) -> Result<()> {
    let mut current = base.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        ensure_not_symlink(&current)?;
    }
    Ok(())
}

fn checked_descendant_path(base: &Path, relative: &Path) -> Result<PathBuf> {
    safe_support_relative(relative)?;
    ensure_no_symlink_components(base, relative)?;
    Ok(base.join(relative))
}

fn artifact_path(dir: &Path, relative: &str) -> Result<PathBuf> {
    checked_descendant_path(dir, relative_artifact_path(relative)?)
}

fn ensure_scope_package_path_safe(scope: &MaterializationScope, slug: &str) -> Result<()> {
    // The host skills dir (`<base>/.claude/skills`) is trusted and may traverse
    // symlinks (dotfile managers); only the slug package directory we create
    // must be a real directory, not a symlink escaping the skills tree.
    let relative = Path::new(slug);
    safe_support_relative(relative)?;
    ensure_no_symlink_components(&scope.skills_dir(), relative)
}

fn support_relative_key(path: &Path) -> Result<String> {
    use std::path::Component;
    safe_support_relative(path)?;
    Ok(path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

fn read_materialization_manifest(dir: &Path, skill_id: &str) -> Result<ManifestState> {
    let path = checked_descendant_path(dir, Path::new(MATERIALIZATION_MANIFEST_FILE))?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ManifestState::Missing);
        }
        Err(err) => return Err(err.into()),
    };
    if !metadata.file_type().is_file() {
        return Ok(ManifestState::Foreign);
    }
    let Ok(manifest) = serde_json::from_str::<MaterializationManifest>(&fs::read_to_string(path)?)
    else {
        return Ok(ManifestState::Foreign);
    };
    if manifest.managed_by != MATERIALIZED_SKILL_MANAGED_BY
        || manifest.skill_id != skill_id
        || !manifest.files.contains_key(SKILL_FILE)
        || manifest
            .files
            .keys()
            .any(|relative| relative_artifact_path(relative).is_err())
    {
        return Ok(ManifestState::Foreign);
    }
    Ok(ManifestState::Owned(manifest))
}

fn hash_bytes(bytes: &[u8]) -> String {
    super::artifact_refs::sha256_bytes(bytes)
}

fn current_artifact_hash(path: &Path) -> Result<Option<String>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    if !metadata.file_type().is_file() {
        return Ok(None);
    }
    Ok(Some(hash_bytes(&fs::read(path)?)))
}

fn artifact_state(dir: &Path, relative: &str, expected_hash: &str) -> Result<ArtifactState> {
    let path = artifact_path(dir, relative)?;
    match current_artifact_hash(&path)? {
        Some(actual) if actual == expected_hash => Ok(ArtifactState::Clean),
        Some(_) => Ok(ArtifactState::Forked),
        None if fs::symlink_metadata(path).is_ok() => Ok(ArtifactState::Forked),
        None => Ok(ArtifactState::Missing),
    }
}

fn desired_artifacts(skill: &ManagedSkill) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        SKILL_FILE.to_string(),
        skill.render_materialized_skill_markdown()?.into_bytes(),
    );
    for support in &skill.support_files {
        let relative = support_relative_key(&support.path)?;
        if relative == SKILL_FILE
            || matches!(
                relative.as_str(),
                MATERIALIZATION_MANIFEST_FILE | MATERIALIZATION_PENDING_FILE
            )
        {
            return Err(config_error(format!(
                "reserved materialized support path '{relative}'"
            )));
        }
        if artifacts
            .insert(relative.clone(), support.bytes.clone())
            .is_some()
        {
            return Err(config_error(format!(
                "duplicate materialized support path '{relative}'"
            )));
        }
    }
    Ok(artifacts)
}

fn write_artifact_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let staging = PathBuf::from(format!("{}.new", path.display()));
    ensure_not_symlink(&staging)?;
    if path_exists_without_following_links(&staging)? {
        if current_artifact_hash(&staging)?.as_deref() != Some(hash_bytes(bytes).as_str()) {
            return Err(config_error(format!(
                "refusing to overwrite foreign materialization staging file '{}'",
                staging.display()
            )));
        }
    } else if let Err(err) = fs::write(&staging, bytes) {
        fs::remove_file(&staging).ok();
        return Err(err.into());
    }
    fs::rename(staging, path)?;
    Ok(())
}

fn write_artifacts(dir: &Path, artifacts: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    for (relative, bytes) in artifacts {
        write_artifact_atomically(&artifact_path(dir, relative)?, bytes)?;
    }
    Ok(())
}

fn build_materialization_manifest(
    skill: &ManagedSkill,
    package_hash: String,
    installation_id: &str,
    artifacts: &BTreeMap<String, Vec<u8>>,
) -> MaterializationManifest {
    MaterializationManifest {
        managed_by: MATERIALIZED_SKILL_MANAGED_BY.to_string(),
        skill_id: skill.metadata.id.clone(),
        package_hash,
        materialized_by: Some(installation_id.to_string()),
        files: artifacts
            .iter()
            .map(|(relative, bytes)| (relative.clone(), hash_bytes(bytes)))
            .collect(),
    }
}

fn write_materialization_manifest(dir: &Path, manifest: &MaterializationManifest) -> Result<()> {
    let value = serde_json::to_value(manifest).map_err(|err| {
        config_error(format!(
            "failed to serialize materialization manifest: {err}"
        ))
    })?;
    let path = checked_descendant_path(dir, Path::new(MATERIALIZATION_MANIFEST_FILE))?;
    ensure_not_symlink(&PathBuf::from(format!("{}.new", path.display())))?;
    crate::agents::safe_write_json_file(&path, &value, None)
}

fn write_pending_materialization(dir: &Path, pending: &PendingMaterialization) -> Result<()> {
    let value = serde_json::to_value(pending).map_err(|err| {
        config_error(format!(
            "failed to serialize pending materialization: {err}"
        ))
    })?;
    let path = checked_descendant_path(dir, Path::new(MATERIALIZATION_PENDING_FILE))?;
    ensure_not_symlink(&PathBuf::from(format!("{}.new", path.display())))?;
    crate::agents::safe_write_json_file(&path, &value, None)
}

fn decode_pending_artifacts(pending: &PendingMaterialization) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut artifacts = BTreeMap::new();
    for (relative, encoded) in &pending.artifacts_hex {
        relative_artifact_path(relative)?;
        let bytes = hex::decode(encoded).map_err(|err| {
            config_error(format!(
                "invalid pending materialization artifact '{relative}': {err}"
            ))
        })?;
        if pending.next_manifest.files.get(relative) != Some(&hash_bytes(&bytes)) {
            return Err(config_error(format!(
                "pending materialization hash mismatch for '{relative}'"
            )));
        }
        artifacts.insert(relative.clone(), bytes);
    }
    if artifacts.keys().ne(pending.next_manifest.files.keys()) {
        return Err(config_error(
            "pending materialization artifact inventory mismatch".to_string(),
        ));
    }
    Ok(artifacts)
}

fn read_pending_materialization(dir: &Path, skill_id: Option<&str>) -> Result<PendingState> {
    let path = checked_descendant_path(dir, Path::new(MATERIALIZATION_PENDING_FILE))?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PendingState::Missing);
        }
        Err(err) => return Err(err.into()),
    };
    if !metadata.file_type().is_file() {
        return Ok(PendingState::Foreign);
    }
    let Ok(pending) = serde_json::from_str::<PendingMaterialization>(&fs::read_to_string(path)?)
    else {
        return Ok(PendingState::Foreign);
    };
    let valid_paths = pending
        .previous_files
        .keys()
        .chain(pending.remove_files.keys())
        .chain(pending.next_manifest.files.keys())
        .all(|relative| relative_artifact_path(relative).is_ok());
    if pending.managed_by != MATERIALIZED_SKILL_MANAGED_BY
        || skill_id.is_some_and(|expected| pending.skill_id != expected)
        || pending.next_manifest.managed_by != MATERIALIZED_SKILL_MANAGED_BY
        || pending.next_manifest.skill_id != pending.skill_id
        || !pending.next_manifest.files.contains_key(SKILL_FILE)
        || !valid_paths
        || decode_pending_artifacts(&pending).is_err()
    {
        return Ok(PendingState::Foreign);
    }
    Ok(PendingState::Owned(Box::new(pending)))
}

fn path_exists_without_following_links(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

fn prune_empty_parents(path: &Path, package_dir: &Path) {
    let mut current = path.parent();
    while let Some(dir) = current {
        if dir == package_dir || fs::remove_dir(dir).is_err() {
            break;
        }
        current = dir.parent();
    }
}

fn remove_clean_artifact(dir: &Path, relative: &str, expected_hash: &str) -> Result<()> {
    if artifact_state(dir, relative, expected_hash)? != ArtifactState::Clean {
        return Ok(());
    }
    let path = artifact_path(dir, relative)?;
    fs::remove_file(&path)?;
    prune_empty_parents(&path, dir);
    Ok(())
}

fn current_artifact_hashes(
    dir: &Path,
    artifacts: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    for relative in artifacts.keys() {
        let path = artifact_path(dir, relative)?;
        if let Some(hash) = current_artifact_hash(&path)? {
            hashes.insert(relative.clone(), hash);
        } else if path_exists_without_following_links(&path)? {
            return Err(config_error(format!(
                "materialized artifact path '{}' is not a regular file",
                path.display()
            )));
        }
    }
    Ok(hashes)
}

fn validate_transaction_paths(
    dir: &Path,
    next_manifest: &MaterializationManifest,
    remove_files: &BTreeMap<String, String>,
) -> Result<()> {
    for relative in next_manifest.files.keys().chain(remove_files.keys()) {
        let path = artifact_path(dir, relative)?;
        ensure_not_symlink(&PathBuf::from(format!("{}.new", path.display())))?;
    }
    Ok(())
}

fn apply_pending_materialization(
    dir: &Path,
    pending: &PendingMaterialization,
) -> Result<MaterializeAction> {
    let artifacts = decode_pending_artifacts(pending)?;
    validate_transaction_paths(dir, &pending.next_manifest, &pending.remove_files)?;

    for (relative, next_hash) in &pending.next_manifest.files {
        let path = artifact_path(dir, relative)?;
        match current_artifact_hash(&path)? {
            Some(current)
                if current == *next_hash
                    || pending.previous_files.get(relative) == Some(&current) => {}
            Some(_) => return Ok(MaterializeAction::SkippedForked),
            None if path_exists_without_following_links(&path)? => {
                return Ok(MaterializeAction::SkippedForked);
            }
            None => {}
        }
    }
    for (relative, expected_hash) in &pending.remove_files {
        let _ = artifact_state(dir, relative, expected_hash)?;
    }

    write_artifacts(dir, &artifacts)?;
    for (relative, expected_hash) in &pending.remove_files {
        remove_clean_artifact(dir, relative, expected_hash)?;
    }
    write_materialization_manifest(dir, &pending.next_manifest)?;

    let path = checked_descendant_path(dir, Path::new(MATERIALIZATION_PENDING_FILE))?;
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    Ok(MaterializeAction::Written)
}

fn commit_materialization_transaction(
    dir: &Path,
    skill: &ManagedSkill,
    package_hash: String,
    installation_id: &str,
    artifacts: &BTreeMap<String, Vec<u8>>,
    previous_files: BTreeMap<String, String>,
    remove_files: BTreeMap<String, String>,
) -> Result<MaterializeAction> {
    let next_manifest =
        build_materialization_manifest(skill, package_hash, installation_id, artifacts);
    validate_transaction_paths(dir, &next_manifest, &remove_files)?;
    if !matches!(
        read_pending_materialization(dir, Some(&skill.metadata.id))?,
        PendingState::Missing
    ) {
        return Err(config_error(format!(
            "materialization transaction already exists for '{}'",
            skill.metadata.id
        )));
    }
    let pending = PendingMaterialization {
        managed_by: MATERIALIZED_SKILL_MANAGED_BY.to_string(),
        skill_id: skill.metadata.id.clone(),
        previous_files,
        remove_files,
        next_manifest,
        artifacts_hex: artifacts
            .iter()
            .map(|(relative, bytes)| (relative.clone(), hex::encode(bytes)))
            .collect(),
    };
    write_pending_materialization(dir, &pending)?;
    apply_pending_materialization(dir, &pending)
}

fn recover_pending_materialization(
    dir: &Path,
    skill_id: Option<&str>,
) -> Result<Option<MaterializeAction>> {
    match read_pending_materialization(dir, skill_id)? {
        PendingState::Missing => Ok(None),
        PendingState::Foreign => Ok(Some(MaterializeAction::SkippedForeign)),
        PendingState::Owned(pending) => Ok(Some(apply_pending_materialization(dir, &pending)?)),
    }
}

fn reconcile_owned_package(
    dir: &Path,
    skill: &ManagedSkill,
    manifest: &MaterializationManifest,
    package_hash: String,
    installation_id: &str,
    artifacts: &BTreeMap<String, Vec<u8>>,
) -> Result<MaterializeAction> {
    // Hash every manifest-tracked file exactly once; the forked gate, the
    // clean check, and the removal candidates below all read from this map
    // instead of re-reading and re-hashing the same files per loop.
    let mut states: BTreeMap<&String, ArtifactState> = BTreeMap::new();
    for (relative, expected_hash) in &manifest.files {
        states.insert(relative, artifact_state(dir, relative, expected_hash)?);
    }
    for relative in artifacts.keys() {
        match states.get(relative) {
            Some(ArtifactState::Forked) => return Ok(MaterializeAction::SkippedForked),
            Some(_) => {}
            None => {
                if path_exists_without_following_links(&artifact_path(dir, relative)?)? {
                    return Ok(MaterializeAction::SkippedForeign);
                }
            }
        }
    }

    let exact_files = manifest.files.keys().eq(artifacts.keys());
    let all_clean = states.values().all(|state| *state == ArtifactState::Clean);
    if manifest.package_hash == package_hash
        && exact_files
        && all_clean
        && manifest.materialized_by.as_deref() == Some(installation_id)
    {
        return Ok(MaterializeAction::Unchanged);
    }

    let mut remove_files = BTreeMap::new();
    for (relative, expected_hash) in &manifest.files {
        if !artifacts.contains_key(relative) && states.get(relative) == Some(&ArtifactState::Clean)
        {
            remove_files.insert(relative.clone(), expected_hash.clone());
        }
    }
    commit_materialization_transaction(
        dir,
        skill,
        package_hash,
        installation_id,
        artifacts,
        manifest.files.clone(),
        remove_files,
    )
}

fn legacy_support_files_are_forked(
    dir: &Path,
    artifacts: &BTreeMap<String, Vec<u8>>,
) -> Result<bool> {
    for (relative, desired) in artifacts {
        if relative == SKILL_FILE {
            continue;
        }
        let path = artifact_path(dir, relative)?;
        if !path_exists_without_following_links(&path)? {
            continue;
        }
        let desired_hash = hash_bytes(desired);
        if current_artifact_hash(&path)?.as_deref() != Some(desired_hash.as_str()) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn initial_support_path_conflicts(
    dir: &Path,
    artifacts: &BTreeMap<String, Vec<u8>>,
) -> Result<bool> {
    for relative in artifacts.keys().filter(|relative| *relative != SKILL_FILE) {
        if path_exists_without_following_links(&artifact_path(dir, relative)?)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn materialized_package_is_forked(
    dir: &Path,
    skill_id: &str,
    provenance: &FileProvenance,
) -> Result<bool> {
    match read_materialization_manifest(dir, skill_id)? {
        ManifestState::Missing => {
            // A lost manifest does not mean a fork: the package may still be
            // pristine in the package-hash domain (#366+) or the legacy
            // body-hash domain (#362). Only a genuine content drift is a fork.
            if recompute_on_disk_package(dir, provenance)?.is_some() {
                Ok(false)
            } else {
                Ok(provenance.is_legacy_forked())
            }
        }
        ManifestState::Foreign => Ok(true),
        ManifestState::Owned(manifest) => {
            for (relative, expected_hash) in &manifest.files {
                if artifact_state(dir, relative, expected_hash)? == ArtifactState::Forked {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

// ---------------------------------------------------------------------------
// Single-skill operations
// ---------------------------------------------------------------------------

/// Materializes one active skill into one scope. Never clobbers a foreign or
/// user-forked file. Idempotent: an already-current managed file is left as
/// [`MaterializeAction::Unchanged`].
pub fn materialize_skill(
    scope: &MaterializationScope,
    skill: &ManagedSkill,
    installation_id: &str,
) -> Result<MaterializeEntry> {
    let slug = skill.host_skill_slug();
    materialize_skill_into(scope, skill, &slug, installation_id)
}

/// Materializes one skill into an explicit host slug. `reconcile_scope` passes a
/// collision-disambiguated slug here; the public entry point uses the skill's
/// own base slug.
fn materialize_skill_into(
    scope: &MaterializationScope,
    skill: &ManagedSkill,
    slug: &str,
    installation_id: &str,
) -> Result<MaterializeEntry> {
    ensure_scope_package_path_safe(scope, slug)?;
    let dir = scope.skill_dir(slug);
    let path = artifact_path(&dir, SKILL_FILE)?;
    let package_hash = skill.materialized_package_hash()?;
    let artifacts = desired_artifacts(skill)?;
    // Hold the per-package lock across the whole read-decide-commit window so a
    // concurrent transaction cannot interleave (see [`PackageLock`]).
    let _lock = lock_package(&dir)?;
    if let Some(action @ (MaterializeAction::SkippedForeign | MaterializeAction::SkippedForked)) =
        recover_pending_materialization(&dir, Some(&skill.metadata.id))?
    {
        return Ok(MaterializeEntry {
            skill_id: skill.metadata.id.clone(),
            path,
            action,
        });
    }
    let provenance = read_file_provenance(&path)?;
    let manifest = read_materialization_manifest(&dir, &skill.metadata.id)?;
    let initial_support_conflict = initial_support_path_conflicts(&dir, &artifacts)?;

    let action = match (&provenance, manifest) {
        (Some(existing), _) if !existing.is_managed() => MaterializeAction::SkippedForeign,
        (_, ManifestState::Foreign) => MaterializeAction::SkippedForeign,
        (_, ManifestState::Owned(manifest)) => {
            fs::create_dir_all(&dir)?;
            reconcile_owned_package(
                &dir,
                skill,
                &manifest,
                package_hash,
                installation_id,
                &artifacts,
            )?
        }
        (Some(existing), ManifestState::Missing) => {
            // Manifest lost (e.g. gitignored/uncommitted sidecar on a fresh
            // clone) but a managed file is on disk. Re-derive from disk: a
            // pristine package-hash (#366+) package is treated as owned and
            // reconciled (re-writing the manifest); otherwise fall back to the
            // legacy body-hash (#362) domain before declaring a user fork.
            if let Some(rederived) = recompute_on_disk_package(&dir, existing)? {
                fs::create_dir_all(&dir)?;
                reconcile_owned_package(
                    &dir,
                    skill,
                    &rederived,
                    package_hash,
                    installation_id,
                    &artifacts,
                )?
            } else if existing.is_legacy_forked()
                || legacy_support_files_are_forked(&dir, &artifacts)?
            {
                MaterializeAction::SkippedForked
            } else {
                fs::create_dir_all(&dir)?;
                let previous_files = current_artifact_hashes(&dir, &artifacts)?;
                commit_materialization_transaction(
                    &dir,
                    skill,
                    package_hash,
                    installation_id,
                    &artifacts,
                    previous_files,
                    BTreeMap::new(),
                )?
            }
        }
        (None, ManifestState::Missing) if initial_support_conflict => {
            MaterializeAction::SkippedForeign
        }
        (None, ManifestState::Missing) => {
            fs::create_dir_all(&dir)?;
            let previous_files = current_artifact_hashes(&dir, &artifacts)?;
            commit_materialization_transaction(
                &dir,
                skill,
                package_hash,
                installation_id,
                &artifacts,
                previous_files,
                BTreeMap::new(),
            )?
        }
    };

    Ok(MaterializeEntry {
        skill_id: skill.metadata.id.clone(),
        path,
        action,
    })
}

fn safe_support_relative(path: &Path) -> Result<&Path> {
    use std::path::Component;
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(config_error(format!(
            "unsafe materialized support path '{}'",
            path.display()
        )));
    }
    for component in path.components() {
        match component {
            Component::Normal(part) if !part.to_string_lossy().contains('\\') => {}
            _ => {
                return Err(config_error(format!(
                    "unsafe materialized support path '{}'",
                    path.display()
                )));
            }
        }
    }
    Ok(path)
}

/// Whether this installation may auto-remove an owned package. Global (home)
/// packages are the local user's own and are always removable. Project-scope
/// packages live inside a repo working tree and are routinely committed and
/// shared, so they are removed only when this installation authored them —
/// never another developer's committed materialization (or a manifest-less
/// package whose author is unknown).
fn may_remove_owned(
    scope: &MaterializationScope,
    manifest: &MaterializationManifest,
    installation_id: &str,
) -> bool {
    !package_is_foreign_to_installation(scope, Some(manifest), installation_id)
}

/// Single source of truth for foreign-installation protection: a project-scope
/// package is foreign unless its manifest records this installation as author.
/// `None` covers missing/unparseable manifests and legacy manifests without
/// `materialized_by`. Global (home) packages are never foreign.
fn package_is_foreign_to_installation(
    scope: &MaterializationScope,
    manifest: Option<&MaterializationManifest>,
    installation_id: &str,
) -> bool {
    match scope.kind {
        MaterializationScopeKind::Global => false,
        MaterializationScopeKind::Project => {
            manifest.and_then(|m| m.materialized_by.as_deref()) != Some(installation_id)
        }
    }
}

/// Removes one materialized skill by slug from one scope. Fork-protected: a
/// user-edited managed file is preserved (and later surfaces as a doctor
/// `Forked` finding); a foreign file is never touched; a committed project-scope
/// package authored by a different installation is left in place.
pub fn remove_materialized_skill(
    scope: &MaterializationScope,
    slug: &str,
    installation_id: &str,
) -> Result<RemoveAction> {
    ensure_scope_package_path_safe(scope, slug)?;
    let dir = scope.skill_dir(slug);
    let path = artifact_path(&dir, SKILL_FILE)?;
    let _lock = lock_package(&dir)?;
    if let Some(action) = recover_pending_materialization(&dir, None)? {
        match action {
            MaterializeAction::SkippedForeign => return Ok(RemoveAction::SkippedForeign),
            MaterializeAction::SkippedForked => return Ok(RemoveAction::SkippedForked),
            MaterializeAction::Written | MaterializeAction::Unchanged => {}
        }
    }
    let existing = match read_file_provenance(&path)? {
        None => return Ok(RemoveAction::Absent),
        Some(existing) if !existing.is_managed() => return Ok(RemoveAction::SkippedForeign),
        Some(existing) => existing,
    };
    let manifest =
        match read_materialization_manifest(&dir, existing.skill_id.as_deref().unwrap_or(slug))? {
            ManifestState::Foreign => return Ok(RemoveAction::SkippedForked),
            ManifestState::Owned(manifest) => manifest,
            ManifestState::Missing => {
                if let Some(rederived) = recompute_on_disk_package(&dir, &existing)? {
                    // Pristine package-hash (#366+) package with a lost manifest.
                    rederived
                } else if existing.is_legacy_forked() {
                    return Ok(RemoveAction::SkippedForked);
                } else {
                    // Pristine legacy (#362) single-file package: synthesize a
                    // manifest so the profile gate and owned-removal path apply.
                    let mut files = BTreeMap::new();
                    if let Some(hash) = current_artifact_hash(&path)? {
                        files.insert(SKILL_FILE.to_string(), hash);
                    }
                    MaterializationManifest {
                        managed_by: MATERIALIZED_SKILL_MANAGED_BY.to_string(),
                        skill_id: existing
                            .skill_id
                            .clone()
                            .unwrap_or_else(|| slug.to_string()),
                        package_hash: existing.content_hash.clone().unwrap_or_default(),
                        materialized_by: None,
                        files,
                    }
                }
            }
        };

    if !may_remove_owned(scope, &manifest, installation_id) {
        return Ok(RemoveAction::SkippedForeign);
    }

    let mut forked = false;
    for (relative, expected_hash) in &manifest.files {
        forked |= artifact_state(&dir, relative, expected_hash)? == ArtifactState::Forked;
    }
    if forked {
        return Ok(RemoveAction::SkippedForked);
    }
    for (relative, expected_hash) in &manifest.files {
        remove_clean_artifact(&dir, relative, expected_hash)?;
    }
    // A re-derived/synthesized manifest may have no sidecar on disk.
    let manifest_path = checked_descendant_path(&dir, Path::new(MATERIALIZATION_MANIFEST_FILE))?;
    match fs::remove_file(&manifest_path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    prune_skill_dir(scope, slug);
    Ok(RemoveAction::Removed)
}

/// Removes the (now empty) skill package directory. Best effort: leftover
/// user-added files keep the directory and are left in place.
fn prune_skill_dir(scope: &MaterializationScope, slug: &str) {
    let dir = scope.skill_dir(slug);
    let _ = fs::remove_dir(dir);
}

// ---------------------------------------------------------------------------
// Scope reconcile + doctor
// ---------------------------------------------------------------------------

/// Short, stable disambiguator derived from a full skill id, used to suffix a
/// host slug when two distinct ids collide on the same base slug.
fn short_id_hash(skill_id: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(skill_id.as_bytes()))[..8].to_string()
}

/// Assigns a host slug to every active skill, disambiguating collisions.
///
/// Distinct managed-skill ids can normalize to the same host slug (`db_sync`
/// vs `db-sync`, ids differing only by a stripped character, or two ids sharing
/// a truncated prefix). Without disambiguation the second skill's package would
/// be seen as `Foreign` and silently never load. Every skill whose base slug is
/// shared by another active skill is suffixed with a short stable hash of its
/// id; non-colliding skills keep their base slug. Returns `(slug, collided)`
/// parallel to `active_skills`.
fn assign_host_slugs(active_skills: &[ManagedSkill]) -> Vec<(String, bool)> {
    let base: Vec<String> = active_skills
        .iter()
        .map(ManagedSkill::host_skill_slug)
        .collect();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for slug in &base {
        *counts.entry(slug.as_str()).or_default() += 1;
    }
    active_skills
        .iter()
        .zip(base.iter())
        .map(|(skill, slug)| {
            if counts.get(slug.as_str()).copied().unwrap_or(0) > 1 {
                (
                    format!("{slug}-{}", short_id_hash(&skill.metadata.id)),
                    true,
                )
            } else {
                (slug.clone(), false)
            }
        })
        .collect()
}

/// Reconciles one scope against the active managed-skill set: materializes
/// every active skill and removes managed files whose skill is no longer
/// active. Fork- and foreign-safe throughout. A single failing package is
/// recorded in `report.errors` and never aborts the rest of the sweep.
pub fn reconcile_scope(
    scope: &MaterializationScope,
    active_skills: &[ManagedSkill],
    installation_id: &str,
) -> Result<ReconcileReport> {
    let mut report = ReconcileReport::default();
    let mut active_slugs = std::collections::BTreeSet::new();

    let slugs = assign_host_slugs(active_skills);
    for (skill, (slug, _collided)) in active_skills.iter().zip(slugs.iter()) {
        active_slugs.insert(slug.clone());
        match materialize_skill_into(scope, skill, slug, installation_id) {
            Ok(entry) => report.materialized.push(entry),
            Err(err) => report
                .errors
                .push(format!("materialize '{}': {err}", skill.metadata.id)),
        }
    }

    for (slug, skill_id) in managed_slugs_in_scope(scope)? {
        if active_slugs.contains(&slug) {
            continue;
        }
        match remove_materialized_skill(scope, &slug, installation_id) {
            Ok(action) => report.removed.push(RemoveEntry {
                skill_id,
                path: scope.skill_md(&slug),
                action,
            }),
            Err(err) => report.errors.push(format!("remove '{skill_id}': {err}")),
        }
    }

    Ok(report)
}

/// Reports drift between the active managed-skill set and one scope's
/// materialized files: missing, forked, conflicting, or orphaned files. Never
/// aborts on a single bad package — a per-skill failure is surfaced as a
/// [`SkillDrift::Warning`] so the rest of the scope's drift is still reported.
pub fn doctor_scope(
    scope: &MaterializationScope,
    active_skills: &[ManagedSkill],
    installation_id: &str,
) -> Vec<SkillDrift> {
    let mut drift = Vec::new();
    let mut active_slugs = std::collections::BTreeSet::new();

    let slugs = assign_host_slugs(active_skills);
    for (skill, (slug, collided)) in active_skills.iter().zip(slugs.iter()) {
        active_slugs.insert(slug.clone());
        let path = scope.skill_md(slug);
        if *collided {
            drift.push(SkillDrift::Warning {
                skill_id: skill.metadata.id.clone(),
                path: path.clone(),
                message: format!(
                    "host slug collides with another active skill; materialized as '{slug}'"
                ),
            });
        }
        match read_file_provenance(&path) {
            Ok(None) => drift.push(SkillDrift::Missing {
                skill_id: skill.metadata.id.clone(),
                path,
            }),
            Ok(Some(existing)) if !existing.is_managed() => drift.push(SkillDrift::Conflict {
                skill_id: skill.metadata.id.clone(),
                path,
            }),
            Ok(Some(existing)) => {
                match materialized_package_is_forked(
                    &scope.skill_dir(slug),
                    &skill.metadata.id,
                    &existing,
                ) {
                    Ok(true) => drift.push(SkillDrift::Forked {
                        skill_id: skill.metadata.id.clone(),
                        path,
                    }),
                    Ok(false) => {}
                    Err(err) => drift.push(SkillDrift::Warning {
                        skill_id: skill.metadata.id.clone(),
                        path,
                        message: format!("materialization check failed: {err}"),
                    }),
                }
            }
            Err(err) => drift.push(SkillDrift::Warning {
                skill_id: skill.metadata.id.clone(),
                path,
                message: format!("materialization check failed: {err}"),
            }),
        }
    }

    match managed_slugs_in_scope(scope) {
        Ok(slugs) => {
            for (slug, skill_id) in slugs {
                if active_slugs.contains(&slug) {
                    continue;
                }
                let dir = scope.skill_dir(&slug);
                // Only an `Owned` manifest carries a trusted author. Missing,
                // foreign, or unreadable manifests all mean the author is
                // unknown — update cannot verify authorship, so treat as `None`.
                let manifest = match read_materialization_manifest(&dir, &skill_id) {
                    Ok(ManifestState::Owned(m)) => Some(m),
                    Ok(ManifestState::Missing | ManifestState::Foreign) | Err(_) => None,
                };
                let path = scope.skill_md(&slug);
                if package_is_foreign_to_installation(scope, manifest.as_ref(), installation_id) {
                    drift.push(SkillDrift::ForeignOrphan { skill_id, path });
                } else {
                    drift.push(SkillDrift::Orphan { skill_id, path });
                }
            }
        }
        Err(err) => drift.push(SkillDrift::Warning {
            skill_id: String::new(),
            path: scope.skills_dir(),
            message: format!("could not enumerate materialized skills: {err}"),
        }),
    }

    drift
}

/// Lists `(slug, skill_id)` for every `TraceDecay`-managed `SKILL.md` currently
/// materialized in a scope's skills directory. Foreign directories (user or
/// repo-local dev skills) are skipped.
fn managed_slugs_in_scope(scope: &MaterializationScope) -> Result<Vec<(String, String)>> {
    let skills_dir = scope.skills_dir();
    let entries = match fs::read_dir(&skills_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let Some(slug) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let skill_md = entry.path().join(SKILL_FILE);
        let Some(provenance) = read_file_provenance(&skill_md)? else {
            continue;
        };
        if !provenance.is_managed() {
            continue;
        }
        let skill_id = provenance.skill_id.unwrap_or_else(|| slug.clone());
        out.push((slug, skill_id));
    }
    out.sort();
    Ok(out)
}

// ---------------------------------------------------------------------------
// Scope detection + profile-driven reconcile
// ---------------------------------------------------------------------------

/// Loads the active managed skills for materialization. Only `Active` skills
/// that target Claude are materialized to Claude scopes, Codex to Codex — the
/// same target filtering the overlay/prompt-index export applies.
fn load_active_managed_skills(profile_root: &Path) -> Result<Vec<ManagedSkill>> {
    crate::automation::skill_targets::load_active_managed_skills(profile_root)
}

fn skills_for_host(skills: &[ManagedSkill], host: MaterializationHost) -> Vec<ManagedSkill> {
    let target = match host {
        MaterializationHost::Claude => crate::automation::skill_targets::SkillInstallTarget::Claude,
        MaterializationHost::Codex => crate::automation::skill_targets::SkillInstallTarget::Codex,
    };
    skills
        .iter()
        .filter(|skill| {
            skill.metadata.state == ManagedSkillState::Active
                && skill.metadata.targets.contains(&target)
        })
        .cloned()
        .collect()
}

/// Skills that should materialize into a given scope. Every active host skill
/// materializes into the user's global scope; only skills explicitly marked
/// project-scoped also materialize into project checkouts, so a single approval
/// never pours untracked `.claude/skills/**` files into every repo the user
/// runs from (and hosts never load duplicate global+project copies).
fn skills_for_scope(skills: &[ManagedSkill], scope: &MaterializationScope) -> Vec<ManagedSkill> {
    let host_skills = skills_for_host(skills, scope.host);
    match scope.kind {
        MaterializationScopeKind::Global => host_skills,
        MaterializationScopeKind::Project => host_skills
            .into_iter()
            .filter(|skill| {
                skill
                    .metadata
                    .materialization_scope
                    .materializes_into_projects()
            })
            .collect(),
    }
}

/// Detects the materialization scopes that actually exist for `home` (global)
/// and `project_root` (project): a scope is eligible when its host config
/// directory (`.claude` / `.codex`) is present, so we never create a host
/// integration the user has not opted into.
pub fn detect_scopes(home: &Path, project_root: &Path) -> Vec<MaterializationScope> {
    let mut scopes = Vec::new();
    for host in MaterializationHost::all() {
        let host_dir = host.skills_subdir().parent().unwrap_or(Path::new(""));
        if home.join(host_dir).is_dir() {
            scopes.push(MaterializationScope::global(host, home));
        }
        if project_root != home && project_root.join(host_dir).is_dir() {
            scopes.push(MaterializationScope::project(host, project_root));
        }
    }
    scopes
}

/// A per-scope reconcile result, tagged with the scope for reporting.
#[derive(Debug, Clone)]
pub struct ScopeReconcileResult {
    pub scope: MaterializationScope,
    pub report: ReconcileReport,
}

/// Reconciles every detected scope against the profile's active managed skills.
/// Returns one result per scope. Errors from a single scope are surfaced in
/// `errors` rather than aborting the whole sweep.
pub fn reconcile_detected_scopes(
    profile_root: &Path,
    home: &Path,
    project_root: &Path,
) -> (Vec<ScopeReconcileResult>, Vec<String>) {
    if !crate::agents::uses_default_user_profile(home, profile_root) {
        return (Vec::new(), Vec::new());
    }
    let mut results = Vec::new();
    let mut errors = Vec::new();
    let skills = match load_active_managed_skills(profile_root) {
        Ok(skills) => skills,
        Err(err) => {
            errors.push(format!("load active managed skills: {err}"));
            return (results, errors);
        }
    };
    let installation = installation_id(profile_root);
    for scope in detect_scopes(home, project_root) {
        let scope_skills = skills_for_scope(&skills, &scope);
        match reconcile_scope(&scope, &scope_skills, &installation) {
            Ok(report) => {
                for error in &report.errors {
                    errors.push(format!("{}: {error}", scope.describe()));
                }
                results.push(ScopeReconcileResult { scope, report });
            }
            Err(err) => errors.push(format!("{}: {err}", scope.describe())),
        }
    }
    (results, errors)
}

/// Resolves the enclosing project root for materialization from a starting
/// directory (usually the process cwd), so running from a subdirectory
/// materializes into the repo root rather than the subdir — and so drift is
/// reported and cleaned up against a single stable root regardless of where the
/// command is run. Prefers the tracedecay-registered project root, then the git
/// worktree/repo checkout root, then falls back to the starting directory.
pub fn resolve_project_root(start: &Path) -> PathBuf {
    crate::config::discover_project_root(start)
        .or_else(|| crate::worktree::git_worktree_root(start))
        .unwrap_or_else(|| start.to_path_buf())
}

/// Non-fatal reconcile for lifecycle call sites (approve, auto-enable, install,
/// update): resolves the profile root from the process environment, reconciles
/// every detected host+scope, and logs (rather than propagates) failures so a
/// materialization problem never breaks an activation or install.
pub fn reconcile_after_activation(profile_root: &Path, project_root: &Path) {
    let Some(home) = crate::agents::home_dir() else {
        return;
    };
    let (_results, errors) = reconcile_detected_scopes(profile_root, &home, project_root);
    for error in errors {
        eprintln!("warning: managed skill materialization failed for {error}");
    }
}

/// Reports materialization drift across every detected scope for `doctor`.
pub fn doctor_detected_scopes(
    profile_root: &Path,
    home: &Path,
    project_root: &Path,
) -> Result<Vec<(MaterializationScope, Vec<SkillDrift>)>> {
    let skills = load_active_managed_skills(profile_root)?;
    let installation = installation_id(profile_root);
    let mut out = Vec::new();
    for scope in detect_scopes(home, project_root) {
        let scope_skills = skills_for_scope(&skills, &scope);
        let drift = doctor_scope(&scope, &scope_skills, &installation);
        out.push((scope.clone(), drift));
    }
    Ok(out)
}
