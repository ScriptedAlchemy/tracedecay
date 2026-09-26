use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::config_error;
use super::host_io::HostIo;
use crate::automation::managed_skills::{
    ManagedSkill, load_active_managed_skills_snapshot, validate_managed_support_files,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::config::{TRACEDECAY_DIR, USER_DATA_DIR_ENV};

const NATIVE_NAMESPACE_DIR: &str = "agent-managed";
const NATIVE_MANIFEST_FILE: &str = ".tracedecay-managed-skills.json";
/// Typed reset authority for a host prompt file's managed-skill index; its
/// reset deletes the refused block from that file.
const MANAGED_SKILL_PROMPT_INDEX_AUTHORITY: &str = "managed skill prompt index";
/// Markers of the released unslugged index block, which this binary neither
/// adopts nor removes. Either one alone (an orphaned half) is the same shape.
const RELEASED_UNSLUGGED_INDEX_MARKERS: [&str; 2] = [
    "<!-- TRACEDECAY MANAGED SKILLS START -->",
    "<!-- TRACEDECAY MANAGED SKILLS END -->",
];
const ALL_SKILL_INSTALL_TARGETS: [SkillInstallTarget; 8] = [
    SkillInstallTarget::Cursor,
    SkillInstallTarget::Codex,
    SkillInstallTarget::Claude,
    SkillInstallTarget::Agents,
    SkillInstallTarget::OpenCode,
    SkillInstallTarget::Kimi,
    SkillInstallTarget::Kiro,
    SkillInstallTarget::Hermes,
];

pub use crate::automation::managed_skills::SkillInstallTarget;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillExportEntry {
    pub id: String,
    pub title: String,
    pub checksum: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInstallSummary {
    pub target: SkillInstallTarget,
    pub output: PathBuf,
    pub exported_count: usize,
    pub exported: Vec<SkillExportEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NativeSkillManifest {
    version: u32,
    target: SkillInstallTarget,
    exported: Vec<SkillExportEntry>,
}

struct RenderedNativeSkillOverlay {
    files: Vec<(PathBuf, Vec<u8>)>,
    exported: Vec<SkillExportEntry>,
}

pub fn install_managed_skills(
    host_io: &HostIo,
    profile_root: &Path,
    target: SkillInstallTarget,
    output: &Path,
) -> Result<SkillInstallSummary> {
    if target.is_native_overlay() {
        export_native_skill_overlay(profile_root, target, output)
    } else {
        export_prompt_skill_index(host_io, profile_root, target, output)
    }
}

pub fn profile_root_for_agent_home(home: &Path) -> PathBuf {
    std::env::var_os(USER_DATA_DIR_ENV)
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(TRACEDECAY_DIR), PathBuf::from)
}

#[hotpath::measure(label = "automation.host_io.export_native_overlay")]
pub fn export_native_skill_overlay(
    profile_root: &Path,
    target: SkillInstallTarget,
    plugin_root: &Path,
) -> Result<SkillInstallSummary> {
    if !target.is_native_overlay() {
        return Err(config_error(format!(
            "{target:?} does not support native skill overlays"
        )));
    }

    let overlay_root = plugin_root.join("skills").join(NATIVE_NAMESPACE_DIR);
    reconcile_overlay_crash_residue(&overlay_root, super::scheduler::foreign_process_is_dead)?;
    let rendered = render_native_skill_overlay(profile_root, target, plugin_root)?;
    if rendered.files.is_empty() {
        if overlay_root.exists() {
            fs::remove_dir_all(&overlay_root)?;
        }
        return Ok(SkillInstallSummary {
            target,
            output: plugin_root.to_path_buf(),
            exported_count: 0,
            exported: Vec::new(),
        });
    }
    let stage_root = unique_overlay_sibling(&overlay_root, "tmp");
    if stage_root.exists() {
        fs::remove_dir_all(&stage_root)?;
    }
    fs::create_dir_all(&stage_root)?;
    let write_result = (|| -> Result<()> {
        for (path, bytes) in &rendered.files {
            let relative =
                path.strip_prefix(&overlay_root)
                    .map_err(|err| TraceDecayError::Config {
                        message: format!(
                            "native skill export path '{}' escaped overlay root '{}': {err}",
                            path.display(),
                            overlay_root.display()
                        ),
                    })?;
            let staged = stage_root.join(relative);
            if let Some(parent) = staged.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(staged, bytes)?;
        }
        Ok(())
    })();
    if let Err(err) = write_result {
        fs::remove_dir_all(&stage_root).ok();
        return Err(err);
    }

    swap_overlay_dirs(&overlay_root, &stage_root)?;
    Ok(SkillInstallSummary {
        target,
        output: plugin_root.to_path_buf(),
        exported_count: rendered.exported.len(),
        exported: rendered.exported,
    })
}

/// Render the complete native managed-skill overlay without mutating it.
///
/// Receipt-backed host lifecycles use this to declare and back up every
/// overlay file before applying the same canonical bytes transactionally.
pub fn rendered_native_skill_overlay_files(
    profile_root: &Path,
    target: SkillInstallTarget,
    plugin_root: &Path,
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    Ok(render_native_skill_overlay(profile_root, target, plugin_root)?.files)
}

fn render_native_skill_overlay(
    profile_root: &Path,
    target: SkillInstallTarget,
    plugin_root: &Path,
) -> Result<RenderedNativeSkillOverlay> {
    if !target.is_native_overlay() {
        return Err(config_error(format!(
            "{target:?} does not support native skill overlays"
        )));
    }
    let skills = load_active_managed_skills_for_target(profile_root, target)?;
    if skills.is_empty() {
        return Ok(RenderedNativeSkillOverlay {
            files: Vec::new(),
            exported: Vec::new(),
        });
    }
    let overlay_root = plugin_root.join("skills").join(NATIVE_NAMESPACE_DIR);
    let mut files = Vec::new();
    let mut exported = Vec::new();
    for skill in skills {
        validate_managed_support_files(&skill.support_files)?;
        let package_dir = overlay_root.join(&skill.metadata.id);
        let skill_path = package_dir.join("SKILL.md");
        files.push((
            skill_path.clone(),
            skill.render_native_skill_markdown()?.into_bytes(),
        ));
        for support in skill.support_files {
            let relative = safe_relative_path(&support.path)?;
            files.push((package_dir.join(relative), support.bytes));
        }
        exported.push(SkillExportEntry {
            id: skill.metadata.id,
            title: skill.metadata.title,
            checksum: skill.metadata.checksum,
            path: skill_path,
        });
    }
    let manifest = NativeSkillManifest {
        version: 1,
        target,
        exported: exported.clone(),
    };
    files.push((
        overlay_root.join(NATIVE_MANIFEST_FILE),
        serde_json::to_vec_pretty(&manifest)?,
    ));
    files.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(RenderedNativeSkillOverlay { files, exported })
}

#[hotpath::measure(label = "automation.host_io.export_prompt_index")]
pub fn export_prompt_skill_index(
    host_io: &HostIo,
    profile_root: &Path,
    target: SkillInstallTarget,
    prompt_path: &Path,
) -> Result<SkillInstallSummary> {
    if target == SkillInstallTarget::Hermes {
        return Err(hermes_host_owned_error());
    }
    let skills = load_active_managed_skills_for_target(profile_root, target)?;
    let existing = match fs::read_to_string(prompt_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };
    refuse_released_unslugged_index(prompt_path, &existing)?;
    let updated = if skills.is_empty() {
        remove_marked_block_for_target(&existing, target)?
    } else {
        let block = render_prompt_index_block(target, &skills);
        replace_or_append_marked_block(&existing, target, &block)?
    };

    if updated != existing {
        if let Some(parent) = prompt_path.parent() {
            fs::create_dir_all(parent)?;
        }
        host_io.safe_write_text_file(prompt_path, &updated)?;
    }

    let exported = skills
        .into_iter()
        .map(|skill| SkillExportEntry {
            id: skill.metadata.id,
            title: skill.metadata.title,
            checksum: skill.metadata.checksum,
            path: prompt_path.to_path_buf(),
        })
        .collect::<Vec<_>>();

    Ok(SkillInstallSummary {
        target,
        output: prompt_path.to_path_buf(),
        exported_count: exported.len(),
        exported,
    })
}

pub fn remove_prompt_skill_index(host_io: &HostIo, prompt_path: &Path) -> Result<()> {
    remove_prompt_skill_indexes(host_io, prompt_path, None)
}

pub fn remove_prompt_skill_index_for_target(
    host_io: &HostIo,
    prompt_path: &Path,
    target: SkillInstallTarget,
) -> Result<()> {
    remove_prompt_skill_indexes(host_io, prompt_path, Some(target))
}

fn remove_prompt_skill_indexes(
    host_io: &HostIo,
    prompt_path: &Path,
    target: Option<SkillInstallTarget>,
) -> Result<()> {
    let existing = match fs::read_to_string(prompt_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    refuse_released_unslugged_index(prompt_path, &existing)?;
    let updated = match target {
        Some(target) => remove_marked_block_for_target(&existing, target)?,
        None => remove_all_marked_blocks(&existing)?,
    };
    if updated == existing {
        return Ok(());
    }
    if updated.trim().is_empty() {
        fs::remove_file(prompt_path)?;
    } else {
        host_io.safe_write_text_file(prompt_path, &updated)?;
    }
    Ok(())
}

pub fn load_active_managed_skills(profile_root: &Path) -> Result<Vec<ManagedSkill>> {
    load_active_managed_skills_snapshot(profile_root)
}

pub fn load_active_managed_skills_for_target(
    profile_root: &Path,
    target: SkillInstallTarget,
) -> Result<Vec<ManagedSkill>> {
    Ok(load_active_managed_skills(profile_root)?
        .into_iter()
        .filter(|skill| skill.metadata.targets.contains(&target))
        .collect())
}

/// Managed-skill ids a host's prompt index still advertises that the profile's
/// skill store no longer holds.
///
/// A non-empty result means the index was written against an earlier store
/// state and no later lifecycle pass reconverged it, so the host is being told
/// about skills `tracedecay_skill_view` can no longer serve.
pub fn stale_prompt_index_ids(
    profile_root: &Path,
    prompt_path: &Path,
    target: SkillInstallTarget,
) -> Result<Vec<String>> {
    let existing = match fs::read_to_string(prompt_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    refuse_released_unslugged_index(prompt_path, &existing)?;
    let (start_marker, end_marker) = prompt_index_markers(target);
    let Some((start, end)) = managed_block_range(&existing, target, &start_marker, &end_marker)?
    else {
        return Ok(Vec::new());
    };
    let active = load_active_managed_skills_for_target(profile_root, target)?
        .into_iter()
        .map(|skill| skill.metadata.id)
        .collect::<std::collections::BTreeSet<_>>();
    Ok(listed_prompt_index_ids(&existing[start..end])
        .filter(|id| !active.contains(id))
        .collect())
}

fn refuse_released_unslugged_index(prompt_path: &Path, existing: &str) -> Result<()> {
    match RELEASED_UNSLUGGED_INDEX_MARKERS
        .into_iter()
        .find(|marker| existing.contains(marker))
    {
        Some(marker) => Err(TraceDecayError::reset_required(
            MANAGED_SKILL_PROMPT_INDEX_AUTHORITY,
            format!(
                "'{}' carries the released unslugged managed-skill index marker `{marker}`",
                prompt_path.display()
            ),
        )),
        None => Ok(()),
    }
}

fn listed_prompt_index_ids(block: &str) -> impl Iterator<Item = String> + '_ {
    block.lines().filter_map(|line| {
        let (id, _) = line.trim_start().strip_prefix("- `")?.split_once('`')?;
        (!id.is_empty()).then(|| id.to_string())
    })
}

fn render_prompt_index_block(target: SkillInstallTarget, skills: &[ManagedSkill]) -> String {
    let mut block = String::new();
    let (start, end) = prompt_index_markers(target);
    block.push_str(&start);
    block.push('\n');
    block.push_str(&prompt_index_preamble(target));

    if skills.is_empty() {
        block.push_str("- No active automatically managed skills are currently exported.\n");
    } else {
        for skill in skills {
            let _ = writeln!(
                block,
                "- `{}`: {}. Summary: {} Full body: `tracedecay_skill_view` with `id=\"{}\"`.",
                skill.metadata.id, skill.metadata.title, skill.metadata.summary, skill.metadata.id
            );
        }
    }

    block.push_str(&end);
    block.push('\n');
    block
}

fn replace_or_append_marked_block(
    existing: &str,
    target: SkillInstallTarget,
    block: &str,
) -> Result<String> {
    let (start_marker, end_marker) = prompt_index_markers(target);
    if let Some((start, end)) = managed_block_range(existing, target, &start_marker, &end_marker)? {
        Ok(splice_range(existing, start, end, block))
    } else {
        let mut updated = String::new();
        updated.push_str(existing.trim_end());
        if !updated.is_empty() {
            updated.push_str("\n\n");
        }
        updated.push_str(block);
        Ok(updated)
    }
}

/// Replace `existing[start..end]` with `block`, normalizing surrounding blank
/// lines and guaranteeing a trailing newline.
fn splice_range(existing: &str, start: usize, end: usize, block: &str) -> String {
    let mut updated = String::new();
    updated.push_str(existing[..start].trim_end());
    updated.push_str("\n\n");
    updated.push_str(block.trim_end());
    updated.push_str("\n\n");
    updated.push_str(existing[end..].trim_start());
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated
}

fn remove_marked_block_for_target(existing: &str, target: SkillInstallTarget) -> Result<String> {
    let (start_marker, end_marker) = prompt_index_markers(target);
    if let Some((start, end)) = managed_block_range(existing, target, &start_marker, &end_marker)? {
        return Ok(remove_range(existing, start, end));
    }
    Ok(existing.to_string())
}

fn remove_all_marked_blocks(existing: &str) -> Result<String> {
    let mut updated = existing.to_string();
    for target in ALL_SKILL_INSTALL_TARGETS
        .into_iter()
        .filter(|target| target.writes_prompt_index())
    {
        let (start_marker, end_marker) = prompt_index_markers(target);
        if let Some((start, end)) =
            managed_block_range(&updated, target, &start_marker, &end_marker)?
        {
            updated = remove_range(&updated, start, end);
        }
    }
    Ok(updated)
}

/// Finds a normal marker-delimited block, or a generated block whose start
/// marker was lost while its exact preamble and end marker remain. Recovery
/// begins at the preamble, so preceding user-authored text is never claimed.
fn managed_block_range(
    existing: &str,
    target: SkillInstallTarget,
    start_marker: &str,
    end_marker: &str,
) -> Result<Option<(usize, usize)>> {
    match (existing.find(start_marker), existing.find(end_marker)) {
        (Some(start), Some(end)) if start <= end => {
            if existing.match_indices(start_marker).count() != 1
                || existing.match_indices(end_marker).count() != 1
            {
                return Err(config_error(
                    "managed skill prompt index markers are ambiguous".to_string(),
                ));
            }
            Ok(Some((start, end + end_marker.len())))
        }
        (None, None) => Ok(None),
        (None, Some(end)) => orphaned_generated_block_range(existing, target, end, end_marker),
        _ => Err(config_error(
            "managed skill prompt index markers are unbalanced".to_string(),
        )),
    }
}

fn orphaned_generated_block_range(
    existing: &str,
    target: SkillInstallTarget,
    end: usize,
    end_marker: &str,
) -> Result<Option<(usize, usize)>> {
    if existing.match_indices(end_marker).count() != 1 {
        return Err(config_error(
            "managed skill prompt index markers are unbalanced".to_string(),
        ));
    }
    let preamble = prompt_index_preamble(target);
    let mut matches = existing[..end].match_indices(&preamble);
    let Some((start, _)) = matches.next() else {
        return Err(config_error(
            "managed skill prompt index markers are unbalanced".to_string(),
        ));
    };
    if matches.next().is_some() || existing[end + end_marker.len()..].contains(&preamble) {
        return Err(config_error(
            "managed skill prompt index markers are unbalanced".to_string(),
        ));
    }
    Ok(Some((start, end + end_marker.len())))
}

fn prompt_index_preamble(target: SkillInstallTarget) -> String {
    format!(
        "## TraceDecay managed skills\n\nThis {} index lists active automatically managed profile skills. For full instructions, call MCP tool `tracedecay_skill_view` with the listed `id`.\n\n",
        target.prompt_label()
    )
}

fn remove_range(existing: &str, start: usize, end: usize) -> String {
    let mut updated = String::new();
    updated.push_str(existing[..start].trim_end());
    updated.push_str("\n\n");
    updated.push_str(existing[end..].trim_start());
    if !updated.trim().is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated
}

fn prompt_index_markers(target: SkillInstallTarget) -> (String, String) {
    let slug = target_marker_slug(target);
    (
        format!("<!-- TRACEDECAY MANAGED SKILLS START {slug} -->"),
        format!("<!-- TRACEDECAY MANAGED SKILLS END {slug} -->"),
    )
}

fn target_marker_slug(target: SkillInstallTarget) -> &'static str {
    match target {
        SkillInstallTarget::Cursor => "cursor",
        SkillInstallTarget::Codex => "codex",
        SkillInstallTarget::Claude => "claude",
        SkillInstallTarget::Agents => "agents",
        SkillInstallTarget::OpenCode => "opencode",
        SkillInstallTarget::Kimi => "kimi",
        SkillInstallTarget::Kiro => "kiro",
        SkillInstallTarget::Hermes => "hermes",
    }
}

fn unique_overlay_sibling(overlay_root: &Path, suffix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    overlay_root.with_file_name(format!(
        ".{NATIVE_NAMESPACE_DIR}.{suffix}-{}-{nonce}",
        std::process::id()
    ))
}

#[derive(Clone, Copy)]
enum OverlaySiblingKind {
    Previous,
    Temporary,
}

fn parse_overlay_sibling(
    overlay_name: &str,
    name: &str,
) -> Option<(OverlaySiblingKind, u32, u128)> {
    let prefix = format!(".{overlay_name}.");
    let rest = name.strip_prefix(&prefix)?;
    let (kind, rest) = if let Some(rest) = rest.strip_prefix("previous-") {
        (OverlaySiblingKind::Previous, rest)
    } else {
        let rest = rest.strip_prefix("tmp-")?;
        (OverlaySiblingKind::Temporary, rest)
    };
    let (pid, nonce) = rest.split_once('-')?;
    Some((kind, pid.parse().ok()?, nonce.parse().ok()?))
}

/// Adopt a dead exporter's backup when the live overlay is missing, and
/// delete that exporter's incomplete stage directories.
///
/// A crash between `overlay → previous` and `stage → overlay` leaves the
/// installed tree only at a uniquely named sibling. The next export must
/// put that tree back before it swaps again. A live PID is left alone: that
/// sibling belongs to an export still in progress.
fn reconcile_overlay_crash_residue(
    overlay_root: &Path,
    owner_is_dead: impl Fn(u32) -> bool,
) -> Result<()> {
    let Some(parent) = overlay_root.parent() else {
        return Ok(());
    };
    let Some(overlay_name) = overlay_root.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(config_error(format!(
                "failed to scan managed skill overlay siblings in '{}': {error}",
                parent.display()
            )));
        }
    };
    let mut adopt = None;
    let mut stale = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            config_error(format!(
                "failed to read managed skill overlay sibling in '{}': {error}",
                parent.display()
            ))
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some((kind, pid, nonce)) = parse_overlay_sibling(overlay_name, &name) else {
            continue;
        };
        if !owner_is_dead(pid) {
            continue;
        }
        let path = entry.path();
        match kind {
            OverlaySiblingKind::Previous if !overlay_root.exists() => {
                if adopt
                    .as_ref()
                    .is_none_or(|(best_nonce, _)| nonce >= *best_nonce)
                {
                    if let Some((_, older)) = adopt.replace((nonce, path)) {
                        stale.push(older);
                    }
                } else {
                    stale.push(path);
                }
            }
            OverlaySiblingKind::Previous | OverlaySiblingKind::Temporary => stale.push(path),
        }
    }
    if let Some((_, backup)) = adopt {
        fs::rename(&backup, overlay_root).map_err(|error| {
            config_error(format!(
                "failed to adopt managed skill overlay backup '{}' onto '{}': {error}",
                backup.display(),
                overlay_root.display()
            ))
        })?;
    }
    for path in stale {
        match fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(config_error(format!(
                    "failed to remove stale managed skill overlay sibling '{}': {error}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn swap_overlay_dirs(overlay_root: &Path, stage_root: &Path) -> Result<()> {
    reconcile_overlay_crash_residue(overlay_root, super::scheduler::foreign_process_is_dead)?;
    let backup_root = unique_overlay_sibling(overlay_root, "previous");
    if backup_root.exists() {
        fs::remove_dir_all(&backup_root)?;
    }
    if overlay_root.exists() {
        fs::rename(overlay_root, &backup_root)?;
    }
    if let Err(err) = fs::rename(stage_root, overlay_root) {
        // Remove the staged directory so a failed swap does not orphan a
        // `.tracedecay-managed.tmp-<pid>-<nonce>` sibling on every retry.
        fs::remove_dir_all(stage_root).ok();
        if backup_root.exists()
            && let Err(restore_err) = fs::rename(&backup_root, overlay_root)
        {
            tracing::warn!(
                backup = %backup_root.display(),
                overlay = %overlay_root.display(),
                error = %restore_err,
                "failed to restore managed skill overlay backup; previous content remains at backup path"
            );
        }
        return Err(err.into());
    }
    if backup_root.exists() {
        fs::remove_dir_all(backup_root)?;
    }
    Ok(())
}

fn safe_relative_path(path: &Path) -> Result<&Path> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(config_error(format!(
            "unsafe managed skill support path '{}'",
            path.display()
        )));
    }
    for component in path.components() {
        match component {
            Component::Normal(part) if !part.to_string_lossy().contains('\\') => {}
            _ => {
                return Err(config_error(format!(
                    "unsafe managed skill support path '{}'",
                    path.display()
                )));
            }
        }
    }
    Ok(path)
}

fn hermes_host_owned_error() -> TraceDecayError {
    config_error(
        "Hermes owns profile skills, pending approvals, usage telemetry, and curator state; TraceDecay does not export managed skills into Hermes",
    )
}

#[cfg(test)]
mod overlay_residue_tests {
    use super::{NATIVE_NAMESPACE_DIR, reconcile_overlay_crash_residue};

    fn plugin_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("plugin root")
    }

    fn overlay_root(plugin: &std::path::Path) -> std::path::PathBuf {
        plugin.join("skills").join(NATIVE_NAMESPACE_DIR)
    }

    #[test]
    fn missing_overlay_adopts_the_newest_dead_pid_backup_and_drops_stale_siblings() {
        let plugin = plugin_root();
        let overlay = overlay_root(plugin.path());
        let skills = overlay.parent().expect("skills dir");
        std::fs::create_dir_all(skills).expect("skills dir");
        let older = skills.join(format!(".{NATIVE_NAMESPACE_DIR}.previous-4242-1"));
        let newer = skills.join(format!(".{NATIVE_NAMESPACE_DIR}.previous-4242-9"));
        let live = skills.join(format!(
            ".{NATIVE_NAMESPACE_DIR}.previous-{}-3",
            std::process::id()
        ));
        let stage = skills.join(format!(".{NATIVE_NAMESPACE_DIR}.tmp-4242-2"));
        std::fs::create_dir_all(older.join("kept")).expect("older backup");
        std::fs::write(older.join("kept").join("SKILL.md"), "older").expect("older body");
        std::fs::create_dir_all(&newer).expect("newer backup");
        std::fs::write(newer.join("SKILL.md"), "newest").expect("newer body");
        std::fs::create_dir_all(&live).expect("live backup");
        std::fs::write(live.join("SKILL.md"), "in progress").expect("live body");
        std::fs::create_dir_all(&stage).expect("dead stage");

        reconcile_overlay_crash_residue(&overlay, |pid| pid == 4242).expect("reconcile");

        assert_eq!(
            std::fs::read_to_string(overlay.join("SKILL.md")).expect("adopted overlay"),
            "newest"
        );
        assert!(
            !older.exists(),
            "an older dead backup is not a second overlay"
        );
        assert!(!stage.exists(), "a dead exporter's stage is incomplete");
        assert!(
            live.is_dir(),
            "a live exporter's backup must not be adopted or deleted"
        );
    }

    #[test]
    fn present_overlay_keeps_its_bytes_and_only_clears_dead_residue() {
        let plugin = plugin_root();
        let overlay = overlay_root(plugin.path());
        std::fs::create_dir_all(&overlay).expect("live overlay");
        std::fs::write(overlay.join("SKILL.md"), "live").expect("live body");
        let renamed = overlay.with_file_name(format!(".{NATIVE_NAMESPACE_DIR}.previous-4242-4"));
        std::fs::create_dir_all(&renamed).expect("dead backup");

        reconcile_overlay_crash_residue(&overlay, |pid| pid == 4242).expect("reconcile");

        assert_eq!(
            std::fs::read_to_string(overlay.join("SKILL.md")).expect("unchanged overlay"),
            "live"
        );
        assert!(!renamed.exists());
    }
}
