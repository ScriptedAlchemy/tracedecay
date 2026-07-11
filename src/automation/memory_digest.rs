//! Durable-facts memory digest exporter (Hermes `MEMORY.md` analogue).
//!
//! Renders a bounded, trust-ranked digest of approved facts from the
//! holographic memory store and delivers it through the same overlay /
//! prompt-index channels that managed skills already use, so curated memory
//! reaches host prompts without requiring an MCP recall call.
//!
//! Data flow:
//!
//! 1. Memory-mutating events (fact proposal apply, curation apply, agent
//!    install) call [`refresh_project_memory_digest`], which selects facts
//!    from the project memory store, renders a per-project section, and
//!    persists it into a profile-level snapshot
//!    (`<profile_root>/agent_managed/memory_digest.json`).
//! 2. Agent install paths call [`sync_memory_digest_export`] with the same
//!    `(profile_root, target, output)` triple used for
//!    `install_managed_skills`, projecting the snapshot into the host channel
//!    and recording the output in a target manifest.
//! 3. After memory mutations, [`export_memory_digest_to_recorded_targets`]
//!    re-projects the fresh snapshot into every recorded output that still
//!    exists, so approved facts deploy without waiting for the next install.
//!
//! Selection policy: trust threshold (default `>= 0.6`), optional category
//! filter, trust-banded ranking (newest first within a band), a global char
//! budget (default ~2000), and hard exclusion of secret-like or
//! prompt-injection-like content.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use libsql::Connection;
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::config_error;
use crate::automation::config::AutomationConfig;
use crate::automation::skill_targets::SkillInstallTarget;
use crate::errors::{Result, TraceDecayError};
use crate::memory::hygiene::detect_secret_like;
use crate::memory::store::MemoryStore;
use crate::memory::types::{FactRecord, MemoryCategory};
use crate::tracedecay::current_timestamp;
use crate::user_config::UserConfig;

pub const MEMORY_DIGEST_START: &str = "<!-- TRACEDECAY MEMORY DIGEST START -->";
pub const MEMORY_DIGEST_END: &str = "<!-- TRACEDECAY MEMORY DIGEST END -->";
const MEMORY_DIGEST_HEADING: &str = "## TraceDecay memory digest";
const MEMORY_DIGEST_BODY_PREAMBLE: &str = "Curated durable facts from TraceDecay project memory (trust-ranked, newest first). \
     For deeper recall call MCP tool `tracedecay_recall`.\n\
     Rate facts you rely on with `tracedecay_fact_feedback` (fact_id, helpful/unhelpful) \
     — flagging a wrong or misleading fact unhelpful matters as much as confirming a \
     helpful one; trust is earned only from this feedback.\n";

/// Minimum trust score for a fact to enter the digest.
pub const DEFAULT_DIGEST_MIN_TRUST: f64 = 0.6;
/// Char budget for the composed digest body (Hermes MEMORY.md analogue).
pub const DEFAULT_DIGEST_CHAR_BUDGET: usize = 2000;

const SNAPSHOT_FILE: &str = "memory_digest.json";
const TARGETS_FILE: &str = "memory_digest_targets.json";
/// How many facts to pull from the store before ranking/filtering.
const FACT_FETCH_LIMIT: usize = 200;
/// Cap on facts kept per project section in the snapshot.
const MAX_SECTION_FACTS: usize = 20;
/// Single rendered fact line cap; longer content is truncated with `...`.
const MAX_FACT_LINE_CHARS: usize = 240;

const CURSOR_RULE_RELATIVE: &str = "rules/tracedecay-memory-digest.mdc";
const CODEX_SKILL_RELATIVE: &str = "skills/agent-managed-memory/SKILL.md";

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryDigestOptions {
    pub min_trust: f64,
    /// When set, only facts in these categories are eligible.
    pub categories: Option<HashSet<MemoryCategory>>,
    pub char_budget: usize,
}

impl Default for MemoryDigestOptions {
    fn default() -> Self {
        Self {
            min_trust: DEFAULT_DIGEST_MIN_TRUST,
            categories: None,
            char_budget: DEFAULT_DIGEST_CHAR_BUDGET,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDigestSection {
    pub project_key: String,
    pub project_label: String,
    /// Pre-rendered, sanitized fact bullet lines, best first.
    pub lines: Vec<String>,
    /// Eligible facts that did not fit the per-section cap.
    #[serde(default)]
    pub omitted_count: usize,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryDigestSnapshot {
    pub version: u32,
    #[serde(default)]
    pub projects: Vec<ProjectDigestSection>,
}

impl Default for MemoryDigestSnapshot {
    fn default() -> Self {
        Self {
            version: 1,
            projects: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DigestTargetEntry {
    target: SkillInstallTarget,
    output: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
struct DigestTargetManifest {
    #[serde(default)]
    targets: Vec<DigestTargetEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryDigestExportSummary {
    pub target: SkillInstallTarget,
    pub output: PathBuf,
    pub fact_count: usize,
    pub char_count: usize,
}

// ---------------------------------------------------------------------------
// Selection and rendering
// ---------------------------------------------------------------------------

/// Ranks facts into trust bands: core (>= 0.85), established (>= 0.7),
/// provisional (>= `min_trust`). Lower band index sorts first.
fn trust_band(trust: f64) -> u8 {
    if trust >= 0.85 {
        0
    } else if trust >= 0.7 {
        1
    } else {
        2
    }
}

fn injection_regexes() -> &'static Vec<(Regex, &'static str)> {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            (
                r"(?i)\b(ignore|disregard|forget|override)\b.{0,40}\b(previous|prior|earlier|above|system)\b.{0,20}\b(instructions?|rules?|prompts?|context)\b",
                "instruction-override phrasing",
            ),
            (
                r"(?i)\bnew\s+(system\s+)?instructions?\s*:",
                "instruction re-declaration",
            ),
            (r"(?i)\bsystem\s+prompt\b", "system-prompt reference"),
            (r"(?i)\byou\s+are\s+now\s+(a|an|the)\b", "persona hijack"),
            (r"<\|im_(start|end)\|>", "chat template control tokens"),
            (r"(?i)</?\s*(system|assistant|developer)\s*>", "role tag markup"),
            (r"(?i)\bdo\s+not\s+(tell|reveal|mention)\b.{0,30}\b(user|human)\b", "concealment directive"),
            // Comment markers could terminate or forge our marked blocks.
            (r"<!--|-->", "html comment markers"),
        ]
        .iter()
        .map(|(pattern, reason)| {
            let regex = match Regex::new(pattern) {
                Ok(regex) => regex,
                Err(err) => panic!("memory digest injection regex must compile: {err}"),
            };
            (regex, *reason)
        })
        .collect()
    })
}

/// Conservative prompt-injection screen for digest content, mirroring how
/// Hermes injection-scans its frozen memory snapshot. Returns a short reason
/// when `content` should be excluded from host prompts.
pub fn detect_injection_like(content: &str) -> Option<String> {
    for (regex, reason) in injection_regexes() {
        if regex.is_match(content) {
            return Some((*reason).to_string());
        }
    }
    None
}

/// Filters and ranks facts for the digest: drops secret-like and
/// injection-like content, applies trust threshold and category filters, and
/// sorts by trust band (best first), then newest first within a band.
pub fn select_digest_facts(
    mut facts: Vec<FactRecord>,
    options: &MemoryDigestOptions,
) -> Vec<FactRecord> {
    facts.retain(|fact| {
        fact.trust_score >= options.min_trust
            && options
                .categories
                .as_ref()
                .is_none_or(|categories| categories.contains(&fact.category))
            && detect_secret_like(&fact.content).is_none()
            && detect_injection_like(&fact.content).is_none()
    });
    facts.sort_by(|left, right| {
        trust_band(left.trust_score)
            .cmp(&trust_band(right.trust_score))
            .then(right.updated_at.cmp(&left.updated_at))
            .then(right.fact_id.cmp(&left.fact_id))
    });
    facts
}

fn flatten_whitespace(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let suffix = "...";
    let mut truncated: String = content
        .chars()
        .take(max_chars.saturating_sub(suffix.len()))
        .collect();
    truncated.push_str(suffix);
    truncated
}

fn render_fact_line(fact: &FactRecord) -> String {
    let content = truncate_chars(&flatten_whitespace(&fact.content), MAX_FACT_LINE_CHARS);
    format!(
        "- ({}, trust {:.2}) {}",
        fact.category.as_str(),
        fact.trust_score,
        content
    )
}

/// Builds the per-project snapshot section from already-fetched facts.
pub fn build_project_section(
    project_key: &str,
    project_label: &str,
    facts: Vec<FactRecord>,
    options: &MemoryDigestOptions,
) -> ProjectDigestSection {
    let selected = select_digest_facts(facts, options);
    let omitted_count = selected.len().saturating_sub(MAX_SECTION_FACTS);
    let lines = selected
        .iter()
        .take(MAX_SECTION_FACTS)
        .map(render_fact_line)
        .collect();
    ProjectDigestSection {
        project_key: project_key.to_string(),
        project_label: project_label.to_string(),
        lines,
        omitted_count,
        updated_at: current_timestamp(),
    }
}

/// Composes the digest body from the snapshot within `char_budget`. Sections
/// are ordered most recently updated first. Returns `None` when the snapshot
/// holds no fact lines at all.
pub fn compose_digest_body(snapshot: &MemoryDigestSnapshot, char_budget: usize) -> Option<String> {
    let mut sections: Vec<&ProjectDigestSection> = snapshot
        .projects
        .iter()
        .filter(|section| !section.lines.is_empty())
        .collect();
    if sections.is_empty() {
        return None;
    }
    sections.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then(left.project_key.cmp(&right.project_key))
    });

    let mut body = String::from(MEMORY_DIGEST_BODY_PREAMBLE);
    let mut budget_hit = false;
    for section in sections {
        let header = format!("\n## {}\n\n", section.project_label);
        if body.len() + header.len() >= char_budget {
            budget_hit = true;
            break;
        }
        body.push_str(&header);
        let mut wrote_line = false;
        for line in &section.lines {
            if body.len() + line.len() + 1 > char_budget {
                budget_hit = true;
                break;
            }
            body.push_str(line);
            body.push('\n');
            wrote_line = true;
        }
        if !wrote_line {
            // Header without any fact line is noise; drop the section header.
            body.truncate(body.len() - header.len());
        }
        if section.omitted_count > 0 && !budget_hit {
            let _ = writeln!(
                body,
                "- (+{} more facts via `tracedecay_recall`)",
                section.omitted_count
            );
        }
        if budget_hit {
            break;
        }
    }
    if budget_hit {
        body.push_str("- (digest truncated at char budget)\n");
    }
    Some(body)
}

// ---------------------------------------------------------------------------
// Snapshot persistence
// ---------------------------------------------------------------------------

fn agent_managed_root(profile_root: &Path) -> PathBuf {
    profile_root.join("agent_managed")
}

pub fn memory_digest_snapshot_path(profile_root: &Path) -> PathBuf {
    agent_managed_root(profile_root).join(SNAPSHOT_FILE)
}

fn digest_targets_path(profile_root: &Path) -> PathBuf {
    agent_managed_root(profile_root).join(TARGETS_FILE)
}

pub fn load_memory_digest_snapshot(profile_root: &Path) -> Result<MemoryDigestSnapshot> {
    let path = memory_digest_snapshot_path(profile_root);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            config_error(format!(
                "failed to parse memory digest snapshot '{}': {e}",
                path.display()
            ))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MemoryDigestSnapshot::default()),
        Err(e) => Err(e.into()),
    }
}

fn save_memory_digest_snapshot(profile_root: &Path, snapshot: &MemoryDigestSnapshot) -> Result<()> {
    let path = memory_digest_snapshot_path(profile_root);
    let contents = serde_json::to_string_pretty(snapshot)?;
    crate::agents::safe_write_text_file(&path, &contents, None)?;
    Ok(())
}

/// Upserts a project's section in the profile-level snapshot. Sections whose
/// selection produced no lines are removed rather than stored empty.
pub fn update_project_digest_section(
    profile_root: &Path,
    section: ProjectDigestSection,
) -> Result<MemoryDigestSnapshot> {
    let mut snapshot = load_memory_digest_snapshot(profile_root)?;
    snapshot
        .projects
        .retain(|existing| existing.project_key != section.project_key);
    if !section.lines.is_empty() {
        snapshot.projects.push(section);
    }
    save_memory_digest_snapshot(profile_root, &snapshot)?;
    Ok(snapshot)
}

/// Removes one project's section from the profile-level snapshot.
pub fn remove_project_digest_section(
    profile_root: &Path,
    project_root: &Path,
) -> Result<MemoryDigestSnapshot> {
    let project_key = project_key_for_root(project_root);
    let mut snapshot = load_memory_digest_snapshot(profile_root)?;
    let before = snapshot.projects.len();
    snapshot
        .projects
        .retain(|existing| existing.project_key != project_key);
    if snapshot.projects.len() != before {
        save_memory_digest_snapshot(profile_root, &snapshot)?;
    }
    Ok(snapshot)
}

fn load_digest_targets(profile_root: &Path) -> DigestTargetManifest {
    let path = digest_targets_path(profile_root);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => DigestTargetManifest::default(),
    }
}

fn save_digest_targets(profile_root: &Path, manifest: &DigestTargetManifest) -> Result<()> {
    let path = digest_targets_path(profile_root);
    let contents = serde_json::to_string_pretty(manifest)?;
    crate::agents::safe_write_text_file(&path, &contents, None)?;
    Ok(())
}

fn record_digest_target(
    profile_root: &Path,
    target: SkillInstallTarget,
    output: &Path,
) -> Result<()> {
    let mut manifest = load_digest_targets(profile_root);
    let entry = DigestTargetEntry {
        target,
        output: output.to_path_buf(),
    };
    if !manifest.targets.contains(&entry) {
        manifest.targets.push(entry);
        save_digest_targets(profile_root, &manifest)?;
    }
    Ok(())
}

fn digest_target_output_matches(recorded: &Path, output: &Path) -> bool {
    if recorded == output {
        return true;
    }
    match (recorded.canonicalize(), output.canonicalize()) {
        (Ok(recorded), Ok(output)) => recorded == output,
        _ => false,
    }
}

fn unrecord_digest_target(
    profile_root: &Path,
    target: SkillInstallTarget,
    output: &Path,
) -> Result<()> {
    let mut manifest = load_digest_targets(profile_root);
    let before = manifest.targets.len();
    manifest.targets.retain(|entry| {
        !(entry.target == target && digest_target_output_matches(&entry.output, output))
    });
    if manifest.targets.len() != before {
        save_digest_targets(profile_root, &manifest)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Config gate
// ---------------------------------------------------------------------------

/// Reads the `automation.export_memory_digest` gate from the user config file
/// in `profile_root` (the `TraceDecay` user data dir). Defaults to enabled when
/// the file or field is missing or unreadable.
pub fn memory_digest_export_enabled(profile_root: &Path) -> bool {
    load_global_automation_config(profile_root).export_memory_digest
}

fn load_global_automation_config(profile_root: &Path) -> AutomationConfig {
    let path = profile_root.join("config.toml");
    let Ok(contents) = fs::read_to_string(&path) else {
        return AutomationConfig::default();
    };
    let Ok(config) = toml::from_str::<UserConfig>(&contents) else {
        return AutomationConfig::default();
    };
    config.automation
}

/// Resolves the effective global+project memory-digest export gate for one
/// project. Project sidecars can override the profile default.
pub async fn memory_digest_export_enabled_for_project(
    profile_root: &Path,
    project_root: &Path,
) -> Result<bool> {
    let global = load_global_automation_config(profile_root);
    let layout = crate::storage::resolve_layout(project_root, profile_root)?;
    let project = crate::automation::config::load_project_config(&layout.dashboard_root).await?;
    let effective = crate::automation::config::effective_config(&global, project.as_ref())?;
    Ok(effective.export_memory_digest)
}

// ---------------------------------------------------------------------------
// Export channels
// ---------------------------------------------------------------------------

fn digest_document(body: &str) -> String {
    format!("# TraceDecay memory digest\n\n{body}")
}

const EMPTY_DIGEST_NOTE: &str = "No durable facts exported yet. Approved facts from TraceDecay project memory will \
     appear here; use MCP tool `tracedecay_recall` for on-demand memory search.\n";

fn native_digest_relative(target: SkillInstallTarget) -> Result<&'static str> {
    match target {
        SkillInstallTarget::Cursor => Ok(CURSOR_RULE_RELATIVE),
        SkillInstallTarget::Codex => Ok(CODEX_SKILL_RELATIVE),
        SkillInstallTarget::Claude
        | SkillInstallTarget::Agents
        | SkillInstallTarget::OpenCode
        | SkillInstallTarget::Kimi
        | SkillInstallTarget::Kiro
        | SkillInstallTarget::Hermes => Err(config_error(format!(
            "{target:?} does not support native memory digest overlays"
        ))),
    }
}

fn render_native_digest_file(target: SkillInstallTarget, body: &str) -> Result<String> {
    match target {
        SkillInstallTarget::Cursor => Ok(format!(
            "---\ndescription: TraceDecay durable memory digest (auto-generated; do not edit)\nalwaysApply: true\n---\n\n{}",
            digest_document(body)
        )),
        SkillInstallTarget::Codex => Ok(format!(
            "---\nname: tracedecay-memory-digest\ndescription: Durable project memory digest exported by TraceDecay. Consult before starting work for approved facts, decisions, and preferences.\n---\n\n{}",
            digest_document(body)
        )),
        SkillInstallTarget::Claude
        | SkillInstallTarget::Agents
        | SkillInstallTarget::OpenCode
        | SkillInstallTarget::Kimi
        | SkillInstallTarget::Kiro
        | SkillInstallTarget::Hermes => Err(config_error(format!(
            "{target:?} does not support native memory digest overlays"
        ))),
    }
}

fn export_native_digest(
    target: SkillInstallTarget,
    plugin_root: &Path,
    body: &str,
) -> Result<PathBuf> {
    let path = plugin_root.join(native_digest_relative(target)?);
    crate::agents::safe_write_text_file(&path, &render_native_digest_file(target, body)?, None)?;
    Ok(path)
}

fn remove_native_digest(target: SkillInstallTarget, plugin_root: &Path) -> Result<()> {
    let path = plugin_root.join(native_digest_relative(target)?);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    // The Codex digest lives in its own skill package dir; prune it when empty.
    if target == SkillInstallTarget::Codex {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
    Ok(())
}

fn render_prompt_digest_block(body: &str) -> String {
    format!("{MEMORY_DIGEST_START}\n{MEMORY_DIGEST_HEADING}\n\n{body}{MEMORY_DIGEST_END}\n")
}

fn replace_or_append_digest_block(existing: &str, block: &str) -> Result<String> {
    if let Some((start, end)) = digest_block_range(existing)? {
        let mut updated = String::new();
        updated.push_str(existing[..start].trim_end());
        if !updated.is_empty() {
            updated.push_str("\n\n");
        }
        updated.push_str(block.trim_end());
        updated.push_str("\n\n");
        updated.push_str(existing[end..].trim_start());
        let trimmed = updated.trim_end().to_string();
        Ok(trimmed + "\n")
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

fn remove_digest_block(existing: &str) -> Result<String> {
    match digest_block_range(existing)? {
        Some((start, end)) => {
            let mut updated = String::new();
            updated.push_str(existing[..start].trim_end());
            updated.push_str("\n\n");
            updated.push_str(existing[end..].trim_start());
            if !updated.trim().is_empty() && !updated.ends_with('\n') {
                updated.push('\n');
            }
            Ok(updated)
        }
        None => Ok(existing.to_string()),
    }
}

fn digest_block_range(existing: &str) -> Result<Option<(usize, usize)>> {
    match (
        existing.find(MEMORY_DIGEST_START),
        existing.find(MEMORY_DIGEST_END),
    ) {
        (Some(start), Some(end)) if start <= end => {
            if existing.match_indices(MEMORY_DIGEST_START).count() != 1
                || existing.match_indices(MEMORY_DIGEST_END).count() != 1
            {
                return Err(config_error(
                    "memory digest prompt markers are ambiguous".to_string(),
                ));
            }
            Ok(Some((start, end + MEMORY_DIGEST_END.len())))
        }
        (None, Some(end)) => {
            if existing.match_indices(MEMORY_DIGEST_END).count() != 1 {
                return Err(config_error(
                    "memory digest prompt markers are unbalanced".to_string(),
                ));
            }
            let preamble = format!("{MEMORY_DIGEST_HEADING}\n\n{MEMORY_DIGEST_BODY_PREAMBLE}");
            let mut matches = existing[..end].match_indices(&preamble);
            let Some((start, _)) = matches.next() else {
                return Err(config_error(
                    "memory digest prompt markers are unbalanced".to_string(),
                ));
            };
            if matches.next().is_some()
                || existing[end + MEMORY_DIGEST_END.len()..].contains(&preamble)
            {
                return Err(config_error(
                    "memory digest prompt markers are unbalanced".to_string(),
                ));
            }
            Ok(Some((start, end + MEMORY_DIGEST_END.len())))
        }
        (None, None) => Ok(None),
        _ => Err(config_error(
            "memory digest prompt markers are unbalanced".to_string(),
        )),
    }
}

fn export_prompt_digest(prompt_path: &Path, body: &str) -> Result<()> {
    let existing = match fs::read_to_string(prompt_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };
    let block = render_prompt_digest_block(body);
    let updated = replace_or_append_digest_block(&existing, &block)?;
    if updated != existing {
        crate::agents::safe_write_text_file(prompt_path, &updated, None)?;
    }
    Ok(())
}

/// Removes the digest block from a prompt-index file, deleting the file when
/// nothing else remains (mirrors `remove_prompt_skill_index`).
pub fn remove_memory_digest_prompt_block(prompt_path: &Path) -> Result<()> {
    let existing = match fs::read_to_string(prompt_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    let updated = remove_digest_block(&existing)?;
    if updated == existing {
        return Ok(());
    }
    if updated.trim().is_empty() {
        fs::remove_file(prompt_path)?;
    } else {
        crate::agents::safe_write_text_file(prompt_path, &updated, None)?;
    }
    Ok(())
}

fn hermes_host_owned_error() -> TraceDecayError {
    config_error(
        "Hermes owns its MEMORY.md snapshot; TraceDecay does not export a memory digest into Hermes",
    )
}

fn native_digest_superseded_by_host_injection(target: SkillInstallTarget) -> bool {
    matches!(
        target,
        SkillInstallTarget::Cursor | SkillInstallTarget::Codex
    )
}

fn native_digest_superseded_error(target: SkillInstallTarget) -> TraceDecayError {
    config_error(format!(
        "{target:?} memory digest export is delivered by the host lifecycle memory injection channel; TraceDecay only cleans up the legacy native digest artifact",
    ))
}

/// Projects the profile snapshot into one host channel, mirroring the
/// `install_managed_skills` `(profile_root, target, output)` contract:
/// native-overlay targets get a rule/skill file inside the plugin root,
/// prompt-index targets get a marked block in the prompt file. When the
/// snapshot is empty a short placeholder is exported instead (so refreshes
/// after new facts can find and update the channel).
pub fn export_memory_digest(
    profile_root: &Path,
    target: SkillInstallTarget,
    output: &Path,
) -> Result<MemoryDigestExportSummary> {
    if target == SkillInstallTarget::Hermes {
        return Err(hermes_host_owned_error());
    }
    if native_digest_superseded_by_host_injection(target) {
        return Err(native_digest_superseded_error(target));
    }
    let snapshot = load_memory_digest_snapshot(profile_root)?;
    let body = compose_digest_body(&snapshot, DEFAULT_DIGEST_CHAR_BUDGET);
    let fact_count = snapshot
        .projects
        .iter()
        .map(|section| section.lines.len())
        .sum();
    let body = body.unwrap_or_else(|| EMPTY_DIGEST_NOTE.to_string());

    let written = if target.is_native_overlay() {
        export_native_digest(target, output, &body)?
    } else {
        export_prompt_digest(output, &body)?;
        output.to_path_buf()
    };
    record_digest_target(profile_root, target, output)?;
    Ok(MemoryDigestExportSummary {
        target,
        output: written,
        fact_count,
        char_count: body.len(),
    })
}

/// Removes the digest artifact for one host channel and forgets the recorded
/// export target so refreshes stop re-creating it.
pub fn remove_memory_digest_export(
    profile_root: &Path,
    target: SkillInstallTarget,
    output: &Path,
) -> Result<()> {
    if target == SkillInstallTarget::Hermes {
        return Ok(());
    }
    if target.is_native_overlay() {
        remove_native_digest(target, output)?;
    } else {
        remove_memory_digest_prompt_block(output)?;
    }
    unrecord_digest_target(profile_root, target, output)
}

/// Install-path entry point: exports when the config gate is enabled,
/// otherwise removes any previously exported digest for this channel.
/// Returns whether a digest artifact now exists for the channel.
pub fn sync_memory_digest_export(
    profile_root: &Path,
    target: SkillInstallTarget,
    output: &Path,
) -> Result<bool> {
    if target == SkillInstallTarget::Hermes {
        return Ok(false);
    }
    if native_digest_superseded_by_host_injection(target) {
        remove_memory_digest_export(profile_root, target, output)?;
        return Ok(false);
    }
    if memory_digest_export_enabled(profile_root) {
        export_memory_digest(profile_root, target, output)?;
        Ok(true)
    } else {
        remove_memory_digest_export(profile_root, target, output)?;
        Ok(false)
    }
}

/// Re-projects the current snapshot into every recorded export target whose
/// host artifact still exists. Native overlays are refreshed when the plugin
/// root directory exists; prompt files only when the file itself exists (a
/// refresh never creates a prompt file the install path did not).
pub fn export_memory_digest_to_recorded_targets(
    profile_root: &Path,
) -> Result<Vec<MemoryDigestExportSummary>> {
    let manifest = load_digest_targets(profile_root);
    let mut summaries = Vec::new();
    for entry in &manifest.targets {
        if native_digest_superseded_by_host_injection(entry.target) {
            remove_memory_digest_export(profile_root, entry.target, &entry.output)?;
            continue;
        }
        let refreshable = if entry.target.is_native_overlay() {
            entry.output.is_dir()
        } else {
            entry.output.is_file()
        };
        if !refreshable {
            continue;
        }
        summaries.push(export_memory_digest(
            profile_root,
            entry.target,
            &entry.output,
        )?);
    }
    Ok(summaries)
}

// ---------------------------------------------------------------------------
// Refresh triggers
// ---------------------------------------------------------------------------

fn project_key_for_root(project_root: &Path) -> String {
    crate::global_db::GlobalDb::canonical_project_key(project_root)
}

fn project_label_for_root(project_root: &Path) -> String {
    project_root.file_name().map_or_else(
        || project_root.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Regenerates the project's digest section from the memory store and
/// re-exports the snapshot into all recorded host channels.
pub async fn refresh_project_memory_digest(
    profile_root: &Path,
    conn: &Connection,
    project_root: &Path,
    options: &MemoryDigestOptions,
) -> Result<()> {
    let category = None;
    let facts = MemoryStore::new(conn)
        .list_facts(category, Some(options.min_trust), FACT_FETCH_LIMIT)
        .await?;
    let section = build_project_section(
        &project_key_for_root(project_root),
        &project_label_for_root(project_root),
        facts,
        options,
    );
    update_project_digest_section(profile_root, section)?;
    export_memory_digest_to_recorded_targets(profile_root)?;
    Ok(())
}

/// Regenerates a project's digest section only when the project's effective
/// automation config allows export. When disabled, any existing section for
/// that project is removed and recorded host channels are refreshed so stale
/// facts disappear from prompts.
pub async fn refresh_memory_digest_after_memory_change_for_profile(
    profile_root: &Path,
    conn: &Connection,
    project_root: &Path,
) -> Result<bool> {
    if !memory_digest_export_enabled_for_project(profile_root, project_root).await? {
        remove_project_digest_section(profile_root, project_root)?;
        export_memory_digest_to_recorded_targets(profile_root)?;
        return Ok(false);
    }
    refresh_project_memory_digest(
        profile_root,
        conn,
        project_root,
        &MemoryDigestOptions::default(),
    )
    .await?;
    Ok(true)
}

/// Non-fatal wrapper for memory-mutating apply paths: resolves the profile
/// root from the environment, honors the config gate, and logs (rather than
/// propagates) failures so digest refresh never breaks an apply.
pub async fn refresh_memory_digest_after_memory_change(conn: &Connection, project_root: &Path) {
    let Some(home) = crate::agents::home_dir() else {
        return;
    };
    let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(&home);
    if let Err(err) =
        refresh_memory_digest_after_memory_change_for_profile(&profile_root, conn, project_root)
            .await
    {
        eprintln!("warning: memory digest refresh failed: {err}");
    }
}
