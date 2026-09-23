//! Claude Code transcript source.
//!
//! Claude Code appends one JSON object per line to
//! `~/.claude/projects/<slug>/<session-uuid>.jsonl` (with subagent transcripts
//! under `…/<session>/subagents/*.jsonl`). Each line carries a top-level `type`
//! (`"user"`/`"assistant"`/…), a `message` object (`role`, `content`, `model`,
//! `id`), an ISO-8601 `timestamp`, the session `cwd`, and `sessionId`/`uuid`.
//!
//! This source discovers transcripts and filters their frames to the current
//! scope by recorded `cwd`, so a project only ingests its own sessions. Retained
//! frames are normalized to canonical envelopes and admitted through the
//! observation pipeline, whose store projector owns the session rows.

use std::path::{Path, PathBuf};

use crate::runtime::shared::{ProjectMembership, ProjectRootMatcherCache, TranscriptScopeMatcher};
use crate::runtime::source::{
    FileDiscoveryLimit, FileDiscoveryReport, JsonlFrameDeferral, TranscriptDiscoveryBounds,
    bound_path_list, collect_files_with_ext_bounded, path_byte_len,
};
use tracedecay_privacy::protect_sensitive_structural_id;
mod cursor;
mod frames;
mod source_records;

use cursor::claude_source_component;
#[cfg(test)]
pub use frames::scan_claude_source_frames;
pub use frames::{
    ClaudeFrameCoverage, ClaudeSkippedFrame, ClaudeSkippedFrameReason, ClaudeSourceFrame,
    ClaudeSourceFrameScan, identify_claude_source, try_scan_claude_source_frames_with_resume,
};
use source_records::record_cwd;
pub use source_records::transcript_cwd;

#[cfg(test)]
use tracedecay_capture::claude::{
    encode_cursor_key as encode_claude_cursor_key, encode_source_id as encode_claude_source_id,
};

const PROVIDER: &str = "claude";

/// `~/.claude/projects/<slug>/<…>.jsonl` is at most a few levels deep.
/// Workflow-nested subagents add `subagents/workflows/wf_<id>/` (three more
/// components) so the scan must reach deeper than a top-level session.
const MAX_SCAN_DEPTH: u8 = 9;
/// `cwd` should appear on an early line; scan a few in case the first is a
/// `summary`/meta line without one.
pub const CWD_PROBE_LINES: usize = 8;

/// Claude Code transcript locator and scope filter.
pub struct ClaudeSource {
    projects_dir: PathBuf,
    user_scope: Option<UserClaudeScope>,
    project_matchers: ProjectRootMatcherCache,
}

struct UserClaudeScope {
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
}

impl ClaudeSource {
    /// Source rooted at the real `~/.claude/projects`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.claude/projects` (used by tests).
    pub fn with_home(home: &Path) -> Self {
        Self {
            projects_dir: home.join(".claude").join("projects"),
            user_scope: None,
            project_matchers: ProjectRootMatcherCache::default(),
        }
    }

    /// Restricts ingestion to transcript rows that cannot be attributed to any
    /// registered project. `session_id` bounds a live hook ingest; `None`
    /// performs a historical sweep.
    #[must_use]
    pub fn for_user_scope(
        mut self,
        session_id: Option<String>,
        registered_roots: Vec<PathBuf>,
    ) -> Self {
        self.user_scope = Some(UserClaudeScope {
            session_id,
            registered_roots,
        });
        self
    }

    /// Retain exactly the frames owned by this source scope and record the
    /// excluded verified ranges for cursor-only persistence.
    pub fn retain_scoped_frames(
        &self,
        scan: &mut ClaudeSourceFrameScan,
        project_root: &Path,
    ) -> Option<Vec<ClaudeSkippedFrame>> {
        if matches!(
            scan.coverage,
            ClaudeFrameCoverage::Deferred {
                reason: JsonlFrameDeferral::Backlog { .. },
                ..
            }
        ) {
            return None;
        }
        let subagent = claude_subagent_identity(&scan.identity.source_path);
        let expected_session_id = self
            .user_scope
            .as_ref()
            .and_then(|scope| scope.session_id.as_deref())
            .map(protect_sensitive_structural_id)
            .transpose()
            .ok()?;
        let parent_session_id = subagent
            .as_ref()
            .map(|info| protect_sensitive_structural_id(&info.parent_session_id))
            .transpose()
            .ok()?;
        if expected_session_id.is_some_and(|expected| {
            expected != scan.identity.session_id
                && parent_session_id.as_deref() != Some(expected.as_str())
        }) {
            return None;
        }

        let scan_start = match scan.coverage {
            ClaudeFrameCoverage::Complete { start_offset, .. }
            | ClaudeFrameCoverage::Deferred { start_offset, .. } => start_offset,
        };
        let session_cwd = (scan_start > 0)
            .then(|| transcript_cwd(&scan.identity.source_path))
            .flatten()
            .or_else(|| {
                if scan_start == 0 {
                    scan.frames
                        .iter()
                        .map(ClaudeSourceFrame::scope_value)
                        .find_map(record_cwd)
                } else {
                    None
                }
            })
            .or_else(|| {
                subagent
                    .as_ref()
                    .and_then(|info| transcript_cwd(&info.parent_transcript_path))
            });
        let scope_matcher = TranscriptScopeMatcher::for_scope_cached(
            project_root,
            self.user_scope
                .as_ref()
                .map(|scope| scope.registered_roots.as_slice()),
            &self.project_matchers,
        );
        // Membership is decided for every frame before any frame is retained
        // or excluded: an `Unknown` (timed-out git identity) must defer the
        // whole scan so no cursor or skip range is persisted for it, and the
        // next sweep retries the identity lookup.
        let mut memberships = Vec::with_capacity(scan.frames.len());
        for frame in &scan.frames {
            let line_cwd = record_cwd(frame.scope_value()).or_else(|| session_cwd.clone());
            let membership = scope_matcher.membership(line_cwd.as_deref());
            if membership == ProjectMembership::Unknown {
                return None;
            }
            memberships.push(membership);
        }
        let mut retained = Vec::with_capacity(scan.frames.len());
        let mut excluded = Vec::new();
        for (frame, membership) in scan.frames.drain(..).zip(memberships) {
            if membership == ProjectMembership::Match {
                retained.push(frame);
            } else {
                excluded.push(ClaudeSkippedFrame {
                    offset: frame.offset,
                    end_offset: frame.end_offset,
                    resume_fingerprint: frame.resume_fingerprint,
                    reason: ClaudeSkippedFrameReason::OutOfScope,
                });
            }
        }
        scan.frames = retained;
        scan.skipped_frames.extend(excluded.iter().copied());
        Some(excluded)
    }

    /// Bounded discovery of this source's transcripts: the scoped session
    /// (and its subagents) for a live ingest, otherwise every project slug.
    /// Frames are filtered by recorded `cwd` afterwards, so discovery need not
    /// replicate Claude's slug-encoding scheme.
    pub fn discover_transcript_paths(
        &self,
        bounds: TranscriptDiscoveryBounds,
    ) -> FileDiscoveryReport {
        if let Some(session_id) = self
            .user_scope
            .as_ref()
            .and_then(|scope| scope.session_id.as_deref())
        {
            return discover_claude_session_scoped_paths(&self.projects_dir, session_id, bounds);
        }
        collect_files_with_ext_bounded(&self.projects_dir, "jsonl", MAX_SCAN_DEPTH, bounds)
    }
}

/// Profile ingestion through an already registered host-admission facade.
pub async fn ingest_user_sessions_with_admission(
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
    admission: &dyn crate::admission::HostAdmission,
) -> crate::runtime::shared::TranscriptIngestStats {
    match crate::runtime::hosts::claude_observation::ingest_user_sessions_with_admission(
        profile_root,
        session_id,
        registered_roots,
        admission,
        None,
        crate::observation::ObservationCancellation::default(),
    )
    .await
    {
        Ok(stats) => stats.transcript,
        Err(error) => {
            let failure = crate::runtime::classify_claude_observation_failure(&error);
            tracing::warn!(
                reason_code = failure.reason_code,
                retryable = failure.retryable,
                "registered Claude ingest failed"
            );
            crate::runtime::shared::TranscriptIngestStats::default()
        }
    }
}

fn discover_claude_session_scoped_paths(
    projects_dir: &Path,
    session_id: &str,
    bounds: TranscriptDiscoveryBounds,
) -> FileDiscoveryReport {
    let mut paths = Vec::new();
    let mut truncated = None;
    let mut skipped_oversized_entries = 0u64;
    let mut bytes_charged = 0u64;
    let Ok(projects) = std::fs::read_dir(projects_dir) else {
        return FileDiscoveryReport {
            paths,
            truncated,
            skipped_oversized_entries,
            bytes_charged,
            files_considered: 0,
        };
    };
    // Stream project slug entries; never collect the full read_dir into a Vec.
    for project_entry in projects.flatten() {
        if truncated.is_some() {
            break;
        }
        let Ok(file_type) = project_entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }
        let project = projects_dir.join(project_entry.file_name());
        let transcript = project.join(format!("{session_id}.jsonl"));
        let remaining = TranscriptDiscoveryBounds {
            max_files: bounds.max_files.saturating_sub(paths.len()),
            max_discovery_bytes: bounds.max_discovery_bytes.saturating_sub(bytes_charged),
            ..bounds
        };
        if remaining.max_files == 0 || remaining.max_discovery_bytes == 0 {
            truncated = Some(FileDiscoveryLimit::FileCount);
            break;
        }
        if transcript.is_file() {
            let path_bytes = path_byte_len(&transcript);
            if path_bytes > remaining.max_path_bytes {
                skipped_oversized_entries = skipped_oversized_entries.saturating_add(1);
            } else {
                let charge = u64::try_from(path_bytes).unwrap_or(u64::MAX);
                if bytes_charged.saturating_add(charge) > bounds.max_discovery_bytes {
                    truncated = Some(FileDiscoveryLimit::DiscoveryBytes);
                    break;
                }
                bytes_charged = bytes_charged.saturating_add(charge);
                paths.push(transcript);
            }
        }
        let subagent_bounds = TranscriptDiscoveryBounds {
            max_files: bounds.max_files.saturating_sub(paths.len()),
            max_discovery_bytes: bounds.max_discovery_bytes.saturating_sub(bytes_charged),
            ..bounds
        };
        if subagent_bounds.max_files == 0 || subagent_bounds.max_discovery_bytes == 0 {
            truncated = Some(FileDiscoveryLimit::FileCount);
            break;
        }
        let subagents = collect_files_with_ext_bounded(
            &project.join(session_id).join("subagents"),
            "jsonl",
            MAX_SCAN_DEPTH,
            subagent_bounds,
        );
        bytes_charged = bytes_charged.saturating_add(subagents.bytes_charged);
        skipped_oversized_entries =
            skipped_oversized_entries.saturating_add(subagents.skipped_oversized_entries);
        paths.extend(subagents.paths);
        if let Some(limit) = subagents.truncated {
            truncated = Some(limit);
            break;
        }
    }
    paths.sort();
    paths.dedup();
    // Re-apply bounds after sort/dedup so materialization stays inside the cap.
    let mut report = bound_path_list(paths, bounds);
    report.truncated = report.truncated.or(truncated);
    report.skipped_oversized_entries = report
        .skipped_oversized_entries
        .saturating_add(skipped_oversized_entries);
    report.bytes_charged = report.bytes_charged.max(bytes_charged);
    crate::runtime::pipeline_metrics::record_sweep_outcome(!report.is_truncated());
    report
}

struct ClaudeSubagentInfo {
    parent_session_id: String,
    parent_transcript_path: PathBuf,
}

/// Detect whether `path` is a subagent transcript and, if so, resolve its
/// parent session linkage.
///
/// A subagent transcript lives somewhere under a `subagents/` directory owned by
/// its parent session:
///
/// * directly spawned: `…/<parent>/subagents/agent-<id>.jsonl`
/// * workflow-nested:   `…/<parent>/subagents/workflows/wf_<run>/agent-<id>.jsonl`
///
/// The parent is always the directory immediately above `subagents/`, so we walk
/// ancestors for a `subagents` component instead of demanding it be the file's
/// immediate parent. That immediate-parent assumption was a bug: workflow-nested
/// subagents failed it and were ingested as orphan standalone sessions.
fn claude_subagent_identity(path: &Path) -> Option<ClaudeSubagentInfo> {
    // Find the `subagents/` ancestor. `ancestors()` yields `path` first, so the
    // file itself can never match the directory name.
    let subagents_dir = path
        .ancestors()
        .find(|anc| anc.file_name().and_then(|name| name.to_str()) == Some("subagents"))?;
    let parent_session_dir = subagents_dir.parent()?;
    let parent_session_id = claude_source_component(parent_session_dir.file_name()?);
    // The parent transcript is the `<parent>.jsonl` sibling of the `<parent>`
    // directory that owns `subagents/`.
    let mut parent_filename = parent_session_dir.file_name()?.to_os_string();
    parent_filename.push(".jsonl");
    let parent_transcript_path = parent_session_dir.parent()?.join(parent_filename);
    Some(ClaudeSubagentInfo {
        parent_session_id,
        parent_transcript_path,
    })
}

#[cfg(test)]
mod tests;
