//! The `freshness:` verdict that opens every search and context response.
//!
//! Two typed authorities feed it and neither is defaulted. The search
//! executor reports, per lane, whether it served the current complete
//! generation or an older one (`CodeIndexLaneStatusV1::Stale`); its ready
//! gate already ran the scheduler's source-freshness ladder. The daemon
//! scheduler registry reports the worktree's staleness-ladder state, whether
//! a rebuild is in flight, and the latest sealed generation. A response is
//! `fresh` only when both agree; anything else is `possibly_stale` with one
//! compact line describing the indexing state, so an agent can decide
//! whether sufficient results are usable without a status preflight.

use std::fmt::Write as _;
use std::path::Path;

use tracedecay_application::retrieval::{
    PrimitiveFreshnessStateV1, PrimitiveIndexingStateV1, PrimitiveSearchFreshnessV1,
};
use tracedecay_dashboard_api::code_index_freshness_api::{
    CodeIndexBuildPhaseV1, CodeIndexFreshnessReader, CodeIndexWorktreeFreshnessV1,
};

use crate::mcp::server::{CodeIndexLaneStatusV1, CodeIndexSearchCoverageV1};

/// What the daemon scheduler registry answered for the project root.
pub(super) enum WorktreeFreshnessSourceV1 {
    /// The registry has a mounted scheduler for this worktree.
    Worktree(Box<CodeIndexWorktreeFreshnessV1>),
    /// The registry is attached but has no mounted scheduler for this root.
    NotMounted,
    /// No scheduler authority is attached to this server (direct mode).
    Unattached,
}

pub(super) async fn read_worktree_freshness(
    reader: Option<&CodeIndexFreshnessReader>,
    project_root: &Path,
) -> WorktreeFreshnessSourceV1 {
    match reader {
        Some(reader) => match hotpath::future!(
            reader(project_root.to_path_buf()),
            label = "mcp.graph.search.freshness"
        )
        .await
        {
            Some(worktree) => WorktreeFreshnessSourceV1::Worktree(Box::new(worktree)),
            None => WorktreeFreshnessSourceV1::NotMounted,
        },
        None => WorktreeFreshnessSourceV1::Unattached,
    }
}

/// The served generation of a completed search, or the typed reason no
/// generation could be served.
pub(super) enum ServedGenerationV1<'a> {
    Served(&'a str),
    Unavailable { reason: &'a str },
}

fn phase_label(phase: CodeIndexBuildPhaseV1) -> &'static str {
    match phase {
        CodeIndexBuildPhaseV1::SourceScan => "source_scan",
        CodeIndexBuildPhaseV1::RelationalPreparation => "relational_preparation",
        CodeIndexBuildPhaseV1::BulkCommit => "bulk_commit",
        CodeIndexBuildPhaseV1::IndexBuild => "index_build",
        CodeIndexBuildPhaseV1::Verification => "verification",
        CodeIndexBuildPhaseV1::Ready => "ready",
    }
}

fn stale_lanes(coverage: &CodeIndexSearchCoverageV1) -> Vec<String> {
    [
        ("exact", &coverage.exact),
        ("lexical", &coverage.lexical),
        ("graph", &coverage.graph),
        ("semantic", &coverage.semantic),
    ]
    .into_iter()
    .filter(|(_, status)| matches!(status, CodeIndexLaneStatusV1::Stale { .. }))
    .map(|(lane, _)| lane.to_owned())
    .collect()
}

/// Derive the verdict from the executor's lane coverage and the scheduler's
/// worktree state.
pub(super) fn search_freshness(
    served: ServedGenerationV1<'_>,
    coverage: &CodeIndexSearchCoverageV1,
    worktree: &WorktreeFreshnessSourceV1,
) -> PrimitiveSearchFreshnessV1 {
    let stale_lanes = stale_lanes(coverage);
    let (served_generation, reason) = match served {
        ServedGenerationV1::Served(generation) => (Some(generation.to_owned()), None),
        ServedGenerationV1::Unavailable { reason } => (None, Some(reason.to_owned())),
    };
    let scheduler_says_stale = match worktree {
        WorktreeFreshnessSourceV1::Worktree(state) => {
            state.staleness_state.as_deref() != Some("fresh")
                || state.rebuild_in_flight
                || (state.latest_generation_id.is_some()
                    && served_generation.is_some()
                    && state.latest_generation_id != served_generation)
        }
        WorktreeFreshnessSourceV1::NotMounted | WorktreeFreshnessSourceV1::Unattached => false,
    };
    if reason.is_none() && stale_lanes.is_empty() && !scheduler_says_stale {
        return PrimitiveSearchFreshnessV1 {
            state: PrimitiveFreshnessStateV1::Fresh,
            indexing: None,
        };
    }

    let mut summary = String::new();
    let (latest_generation, staleness_state, rebuild_in_flight) = match worktree {
        WorktreeFreshnessSourceV1::Worktree(state) => {
            let _ = write!(
                summary,
                "state={} rebuild_in_flight={}",
                state.staleness_state.as_deref().unwrap_or("unknown"),
                state.rebuild_in_flight
            );
            (
                state.latest_generation_id.clone(),
                state.staleness_state.clone(),
                Some(state.rebuild_in_flight),
            )
        }
        WorktreeFreshnessSourceV1::NotMounted => {
            summary.push_str("scheduler=not_mounted");
            (None, None, None)
        }
        WorktreeFreshnessSourceV1::Unattached => {
            summary.push_str("scheduler=unattached");
            (None, None, None)
        }
    };
    let _ = write!(
        summary,
        " served_generation={}",
        served_generation.as_deref().unwrap_or("none")
    );
    if let Some(latest) = latest_generation.as_deref() {
        let _ = write!(summary, " latest_generation={latest}");
    }
    if let WorktreeFreshnessSourceV1::Worktree(state) = worktree {
        if let Some(hints) = state.hook_hint_count.filter(|count| *count > 0) {
            let _ = write!(summary, " pending_hook_hints={hints}");
        }
        if let Some(progress) = &state.progress {
            let _ = write!(
                summary,
                " progress={} {}/{} files",
                phase_label(progress.phase),
                progress.completed_files,
                progress.total_files
            );
        }
    }
    if !stale_lanes.is_empty() {
        let _ = write!(summary, " stale_lanes={}", stale_lanes.join(","));
    }
    if let Some(reason) = reason.as_deref() {
        let _ = write!(summary, " unavailable={reason}");
    }
    PrimitiveSearchFreshnessV1 {
        state: PrimitiveFreshnessStateV1::PossiblyStale,
        indexing: Some(PrimitiveIndexingStateV1 {
            summary,
            served_generation,
            latest_generation,
            staleness_state,
            rebuild_in_flight,
            stale_lanes,
            reason,
        }),
    }
}

/// The opening lines of a rendered response: the verdict, plus the indexing
/// state when the verdict is `possibly_stale`.
pub(super) fn freshness_lines(freshness: &PrimitiveSearchFreshnessV1) -> String {
    let mut lines = format!("freshness: {}\n", freshness.state.as_str());
    if let Some(indexing) = &freshness.indexing {
        let _ = writeln!(lines, "indexing: {}", indexing.summary);
    }
    lines
}

#[cfg(test)]
mod tests {
    use tracedecay_dashboard_api::code_index_freshness_api::CodeIndexBuildProgressV1;

    use super::*;

    fn worktree(
        staleness_state: &str,
        rebuild_in_flight: bool,
        latest_generation_id: Option<&str>,
    ) -> WorktreeFreshnessSourceV1 {
        WorktreeFreshnessSourceV1::Worktree(Box::new(CodeIndexWorktreeFreshnessV1 {
            worktree_root: "/fixture".to_owned(),
            latest_generation_id: latest_generation_id.map(str::to_owned),
            staleness_state: Some(staleness_state.to_owned()),
            rebuild_in_flight,
            hook_hint_count: Some(0),
            coverage: "complete".to_owned(),
            ..CodeIndexWorktreeFreshnessV1::default()
        }))
    }

    #[test]
    fn settled_generation_is_fresh_when_lanes_and_scheduler_agree() {
        let freshness = search_freshness(
            ServedGenerationV1::Served("generation.1"),
            &CodeIndexSearchCoverageV1::warm(),
            &worktree("fresh", false, Some("generation.1")),
        );
        assert_eq!(freshness.state, PrimitiveFreshnessStateV1::Fresh);
        assert!(freshness.indexing.is_none());
        assert_eq!(freshness_lines(&freshness), "freshness: fresh\n");
    }

    #[test]
    fn executor_stale_lanes_are_possibly_stale_even_without_a_scheduler() {
        let semantic = crate::mcp::server::CodeIndexSemanticStatusV1::Complete;
        let coverage = CodeIndexSearchCoverageV1::fused_stale("generation.0", &semantic);
        let freshness = search_freshness(
            ServedGenerationV1::Served("generation.0"),
            &coverage,
            &WorktreeFreshnessSourceV1::Unattached,
        );
        assert_eq!(freshness.state, PrimitiveFreshnessStateV1::PossiblyStale);
        let indexing = freshness.indexing.as_ref().expect("indexing line");
        assert_eq!(indexing.stale_lanes, ["exact", "lexical", "graph"]);
        assert_eq!(
            indexing.summary,
            "scheduler=unattached served_generation=generation.0 stale_lanes=exact,lexical,graph"
        );
        assert_eq!(
            freshness_lines(&freshness),
            "freshness: possibly_stale\nindexing: scheduler=unattached served_generation=generation.0 stale_lanes=exact,lexical,graph\n"
        );
    }

    #[test]
    fn scheduler_rebuild_or_newer_generation_marks_a_warm_page_possibly_stale() {
        let warm = CodeIndexSearchCoverageV1::warm();
        let refreshing = search_freshness(
            ServedGenerationV1::Served("generation.1"),
            &warm,
            &worktree("refreshing", true, Some("generation.1")),
        );
        assert_eq!(refreshing.state, PrimitiveFreshnessStateV1::PossiblyStale);
        assert_eq!(
            refreshing
                .indexing
                .as_ref()
                .map(|indexing| indexing.summary.as_str()),
            Some(
                "state=refreshing rebuild_in_flight=true served_generation=generation.1 latest_generation=generation.1"
            )
        );

        let newer = search_freshness(
            ServedGenerationV1::Served("generation.1"),
            &warm,
            &worktree("fresh", false, Some("generation.2")),
        );
        assert_eq!(newer.state, PrimitiveFreshnessStateV1::PossiblyStale);
        assert_eq!(
            newer
                .indexing
                .as_ref()
                .and_then(|indexing| indexing.latest_generation.as_deref()),
            Some("generation.2")
        );

        let unattached = search_freshness(
            ServedGenerationV1::Served("generation.1"),
            &warm,
            &WorktreeFreshnessSourceV1::Unattached,
        );
        assert_eq!(
            unattached.state,
            PrimitiveFreshnessStateV1::Fresh,
            "the executor's current-generation claim stands when no scheduler authority is attached"
        );
    }

    #[test]
    fn unavailable_search_reports_the_indexing_state_with_progress() {
        let mut state = CodeIndexWorktreeFreshnessV1 {
            worktree_root: "/fixture".to_owned(),
            staleness_state: Some("indexing".to_owned()),
            rebuild_in_flight: true,
            hook_hint_count: Some(3),
            coverage: "complete".to_owned(),
            ..CodeIndexWorktreeFreshnessV1::default()
        };
        state.progress = Some(CodeIndexBuildProgressV1 {
            generation_id: "generation.next".to_owned(),
            daemon_incarnation: 1,
            producer_incarnation: 1,
            progress_epoch: 1,
            sealed_source_digest: "sha256:fixture".to_owned(),
            phase: CodeIndexBuildPhaseV1::BulkCommit,
            committed_pages: 0,
            committed_chunks: 0,
            committed_imports: 0,
            committed_payload_bytes: 0,
            completed_files: 250,
            total_files: 500,
            completed_lexical_bytes: 0,
            total_lexical_bytes: 0,
            current_batch_pages: 0,
            current_batch_payload_bytes: 0,
            elapsed_micros: 0,
            last_commit_latency_micros: None,
            files_per_second: None,
            lexical_bytes_per_second: None,
            estimated_remaining_seconds: None,
            last_progress_micros: 0,
            blocked_reason: None,
        });
        let freshness = search_freshness(
            ServedGenerationV1::Unavailable {
                reason: "generation_unavailable",
            },
            &CodeIndexSearchCoverageV1::unavailable("generation_rebuilding"),
            &WorktreeFreshnessSourceV1::Worktree(Box::new(state)),
        );
        assert_eq!(freshness.state, PrimitiveFreshnessStateV1::PossiblyStale);
        let indexing = freshness.indexing.expect("indexing line");
        assert_eq!(
            indexing.summary,
            "state=indexing rebuild_in_flight=true served_generation=none pending_hook_hints=3 progress=bulk_commit 250/500 files unavailable=generation_unavailable"
        );
        assert_eq!(indexing.reason.as_deref(), Some("generation_unavailable"));
        assert!(indexing.stale_lanes.is_empty());
    }
}
