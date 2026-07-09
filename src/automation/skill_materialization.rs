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
}

impl SkillDrift {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Missing { .. } => "missing",
            Self::Forked { .. } => "forked",
            Self::Conflict { .. } => "conflict",
            Self::Orphan { .. } => "orphan",
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Missing { path, .. }
            | Self::Forked { path, .. }
            | Self::Conflict { path, .. }
            | Self::Orphan { path, .. } => path,
        }
    }

    pub fn skill_id(&self) -> &str {
        match self {
            Self::Missing { skill_id, .. }
            | Self::Forked { skill_id, .. }
            | Self::Conflict { skill_id, .. }
            | Self::Orphan { skill_id, .. } => skill_id,
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

    /// A managed file is forked when the body on disk no longer hashes to the
    /// `content-hash` we recorded when we wrote it.
    fn is_legacy_forked(&self) -> bool {
        match (&self.content_hash, &self.body_hash) {
            (Some(recorded), Some(actual)) => recorded != actual,
            // A managed file missing a content-hash is treated as forked so we
            // never silently overwrite something we cannot verify we authored.
            _ => true,
        }
    }
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

fn ensure_no_symlink_components(base: &Path, relative: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in base.components() {
        current.push(component.as_os_str());
        ensure_not_symlink(&current)?;
    }
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
    let relative = Path::new(scope.host.skills_subdir()).join(slug);
    safe_support_relative(&relative)?;
    ensure_no_symlink_components(&scope.base_dir, &relative)
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
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
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
    artifacts: &BTreeMap<String, Vec<u8>>,
) -> MaterializationManifest {
    MaterializationManifest {
        managed_by: MATERIALIZED_SKILL_MANAGED_BY.to_string(),
        skill_id: skill.metadata.id.clone(),
        package_hash,
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
    artifacts: &BTreeMap<String, Vec<u8>>,
    previous_files: BTreeMap<String, String>,
    remove_files: BTreeMap<String, String>,
) -> Result<MaterializeAction> {
    let next_manifest = build_materialization_manifest(skill, package_hash, artifacts);
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
    artifacts: &BTreeMap<String, Vec<u8>>,
) -> Result<MaterializeAction> {
    for relative in artifacts.keys() {
        if let Some(expected_hash) = manifest.files.get(relative) {
            if artifact_state(dir, relative, expected_hash)? == ArtifactState::Forked {
                return Ok(MaterializeAction::SkippedForked);
            }
        } else if path_exists_without_following_links(&artifact_path(dir, relative)?)? {
            return Ok(MaterializeAction::SkippedForeign);
        }
    }

    let exact_files = manifest.files.keys().eq(artifacts.keys());
    let mut all_clean = true;
    for (relative, expected_hash) in &manifest.files {
        all_clean &= artifact_state(dir, relative, expected_hash)? == ArtifactState::Clean;
    }
    if manifest.package_hash == package_hash && exact_files && all_clean {
        return Ok(MaterializeAction::Unchanged);
    }

    let mut remove_files = BTreeMap::new();
    for (relative, expected_hash) in &manifest.files {
        if !artifacts.contains_key(relative)
            && artifact_state(dir, relative, expected_hash)? == ArtifactState::Clean
        {
            remove_files.insert(relative.clone(), expected_hash.clone());
        }
    }
    commit_materialization_transaction(
        dir,
        skill,
        package_hash,
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
        ManifestState::Missing => Ok(provenance.is_legacy_forked()),
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
) -> Result<MaterializeEntry> {
    let slug = skill.host_skill_slug();
    ensure_scope_package_path_safe(scope, &slug)?;
    let dir = scope.skill_dir(&slug);
    let path = artifact_path(&dir, SKILL_FILE)?;
    let package_hash = skill.materialized_package_hash()?;
    let artifacts = desired_artifacts(skill)?;
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
            reconcile_owned_package(&dir, skill, &manifest, package_hash, &artifacts)?
        }
        (Some(existing), ManifestState::Missing) if existing.is_legacy_forked() => {
            MaterializeAction::SkippedForked
        }
        (Some(_), ManifestState::Missing) if legacy_support_files_are_forked(&dir, &artifacts)? => {
            MaterializeAction::SkippedForked
        }
        (None, ManifestState::Missing) if initial_support_conflict => {
            MaterializeAction::SkippedForeign
        }
        _ => {
            fs::create_dir_all(&dir)?;
            let previous_files = current_artifact_hashes(&dir, &artifacts)?;
            commit_materialization_transaction(
                &dir,
                skill,
                package_hash,
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

/// Removes one materialized skill by slug from one scope. Fork-protected: a
/// user-edited managed file is preserved (and later surfaces as a doctor
/// `Forked` finding); a foreign file is never touched.
pub fn remove_materialized_skill(scope: &MaterializationScope, slug: &str) -> Result<RemoveAction> {
    ensure_scope_package_path_safe(scope, slug)?;
    let dir = scope.skill_dir(slug);
    let path = artifact_path(&dir, SKILL_FILE)?;
    if let Some(action) = recover_pending_materialization(&dir, None)? {
        match action {
            MaterializeAction::SkippedForeign => return Ok(RemoveAction::SkippedForeign),
            MaterializeAction::SkippedForked => return Ok(RemoveAction::SkippedForked),
            MaterializeAction::Written | MaterializeAction::Unchanged => {}
        }
    }
    let action = match read_file_provenance(&path)? {
        None => RemoveAction::Absent,
        Some(existing) if !existing.is_managed() => RemoveAction::SkippedForeign,
        Some(existing) => {
            match read_materialization_manifest(&dir, existing.skill_id.as_deref().unwrap_or(slug))?
            {
                ManifestState::Foreign => RemoveAction::SkippedForked,
                ManifestState::Missing if existing.is_legacy_forked() => {
                    RemoveAction::SkippedForked
                }
                ManifestState::Missing => {
                    fs::remove_file(&path)?;
                    prune_skill_dir(scope, slug);
                    RemoveAction::Removed
                }
                ManifestState::Owned(manifest) => {
                    let mut forked = false;
                    for (relative, expected_hash) in &manifest.files {
                        forked |=
                            artifact_state(&dir, relative, expected_hash)? == ArtifactState::Forked;
                    }
                    if forked {
                        RemoveAction::SkippedForked
                    } else {
                        for (relative, expected_hash) in &manifest.files {
                            remove_clean_artifact(&dir, relative, expected_hash)?;
                        }
                        fs::remove_file(checked_descendant_path(
                            &dir,
                            Path::new(MATERIALIZATION_MANIFEST_FILE),
                        )?)?;
                        prune_skill_dir(scope, slug);
                        RemoveAction::Removed
                    }
                }
            }
        }
    };
    Ok(action)
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

/// Reconciles one scope against the active managed-skill set: materializes
/// every active skill and removes managed files whose skill is no longer
/// active. Fork- and foreign-safe throughout.
pub fn reconcile_scope(
    scope: &MaterializationScope,
    active_skills: &[ManagedSkill],
) -> Result<ReconcileReport> {
    let mut report = ReconcileReport::default();
    let mut active_slugs = std::collections::BTreeSet::new();

    for skill in active_skills {
        active_slugs.insert(skill.host_skill_slug());
        report.materialized.push(materialize_skill(scope, skill)?);
    }

    for (slug, skill_id) in managed_slugs_in_scope(scope)? {
        if active_slugs.contains(&slug) {
            continue;
        }
        let action = remove_materialized_skill(scope, &slug)?;
        report.removed.push(RemoveEntry {
            skill_id,
            path: scope.skill_md(&slug),
            action,
        });
    }

    Ok(report)
}

/// Reports drift between the active managed-skill set and one scope's
/// materialized files: missing, forked, conflicting, or orphaned files.
pub fn doctor_scope(
    scope: &MaterializationScope,
    active_skills: &[ManagedSkill],
) -> Result<Vec<SkillDrift>> {
    let mut drift = Vec::new();
    let mut active_slugs = std::collections::BTreeSet::new();

    for skill in active_skills {
        let slug = skill.host_skill_slug();
        active_slugs.insert(slug.clone());
        let path = scope.skill_md(&slug);
        match read_file_provenance(&path)? {
            None => drift.push(SkillDrift::Missing {
                skill_id: skill.metadata.id.clone(),
                path,
            }),
            Some(existing) if !existing.is_managed() => drift.push(SkillDrift::Conflict {
                skill_id: skill.metadata.id.clone(),
                path,
            }),
            Some(existing)
                if materialized_package_is_forked(
                    &scope.skill_dir(&slug),
                    &skill.metadata.id,
                    &existing,
                )? =>
            {
                drift.push(SkillDrift::Forked {
                    skill_id: skill.metadata.id.clone(),
                    path,
                });
            }
            Some(_) => {}
        }
    }

    for (slug, skill_id) in managed_slugs_in_scope(scope)? {
        if active_slugs.contains(&slug) {
            continue;
        }
        drift.push(SkillDrift::Orphan {
            skill_id,
            path: scope.skill_md(&slug),
        });
    }

    Ok(drift)
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
    let mut results = Vec::new();
    let mut errors = Vec::new();
    let skills = match load_active_managed_skills(profile_root) {
        Ok(skills) => skills,
        Err(err) => {
            errors.push(format!("load active managed skills: {err}"));
            return (results, errors);
        }
    };
    for scope in detect_scopes(home, project_root) {
        let host_skills = skills_for_host(&skills, scope.host);
        match reconcile_scope(&scope, &host_skills) {
            Ok(report) => results.push(ScopeReconcileResult { scope, report }),
            Err(err) => errors.push(format!("{}: {err}", scope.describe())),
        }
    }
    (results, errors)
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
    let mut out = Vec::new();
    for scope in detect_scopes(home, project_root) {
        let host_skills = skills_for_host(&skills, scope.host);
        out.push((scope.clone(), doctor_scope(&scope, &host_skills)?));
    }
    Ok(out)
}
