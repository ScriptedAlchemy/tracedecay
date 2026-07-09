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

impl FileProvenance {
    fn is_managed(&self) -> bool {
        self.managed_by.as_deref() == Some(MATERIALIZED_SKILL_MANAGED_BY)
    }

    /// A managed file is forked when the body on disk no longer hashes to the
    /// `content-hash` we recorded when we wrote it.
    fn is_forked(&self) -> bool {
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
    let path = scope.skill_md(&slug);

    let action = match read_file_provenance(&path)? {
        Some(existing) if !existing.is_managed() => MaterializeAction::SkippedForeign,
        Some(existing) if existing.is_forked() => MaterializeAction::SkippedForked,
        Some(existing)
            if existing.content_hash.as_deref() == Some(&skill.materialized_body_hash()) =>
        {
            MaterializeAction::Unchanged
        }
        _ => {
            write_skill_file(scope, skill, &slug)?;
            MaterializeAction::Written
        }
    };

    Ok(MaterializeEntry {
        skill_id: skill.metadata.id.clone(),
        path,
        action,
    })
}

fn write_skill_file(scope: &MaterializationScope, skill: &ManagedSkill, slug: &str) -> Result<()> {
    let dir = scope.skill_dir(slug);
    fs::create_dir_all(&dir)?;
    let markdown = skill.render_materialized_skill_markdown()?;
    crate::agents::safe_write_text_file(&dir.join(SKILL_FILE), &markdown, None)?;
    write_support_files(&dir, skill)?;
    Ok(())
}

fn write_support_files(dir: &Path, skill: &ManagedSkill) -> Result<()> {
    for support in &skill.support_files {
        let relative = safe_support_relative(&support.path)?;
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, &support.bytes)?;
    }
    Ok(())
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
    let path = scope.skill_md(slug);
    let action = match read_file_provenance(&path)? {
        None => RemoveAction::Absent,
        Some(existing) if !existing.is_managed() => RemoveAction::SkippedForeign,
        Some(existing) if existing.is_forked() => RemoveAction::SkippedForked,
        Some(_) => {
            fs::remove_file(&path)?;
            prune_skill_dir(scope, slug);
            RemoveAction::Removed
        }
    };
    Ok(action)
}

/// Removes the (now empty) skill package directory. Best effort: leftover
/// user-added files keep the directory and are left in place.
fn prune_skill_dir(scope: &MaterializationScope, slug: &str) {
    let dir = scope.skill_dir(slug);
    remove_dir_if_only_managed_leftovers(&dir);
}

fn remove_dir_if_only_managed_leftovers(dir: &Path) {
    // Try a plain remove first (empty dir). If support files remain that we
    // wrote, remove the whole package. We only reach here after deleting a
    // managed SKILL.md, so the package is ours.
    if fs::remove_dir(dir).is_ok() {
        return;
    }
    let _ = fs::remove_dir_all(dir);
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
            Some(existing) if existing.is_forked() => drift.push(SkillDrift::Forked {
                skill_id: skill.metadata.id.clone(),
                path,
            }),
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
