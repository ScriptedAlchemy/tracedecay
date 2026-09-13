//! Before/after harness for bounded Git-evidence reads.
//!
//! Seeds one verified Git-evidence generation per store size, then answers the
//! retained-session read shapes two ways: full recovery of the projection
//! followed by the in-memory query (what every facade call ran before), and
//! the indexed graph view. Reports median latency plus the graph store's
//! row-decode counters for each, so "scales with the answer, not the store" is
//! a measurement rather than a claim. Prints one JSON object to stdout.
//!
//! ```sh
//! cargo bench -p tracedecay-sessions --bench git_evidence_bounded_reads
//! ```

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracedecay_domain::{BrainId, LocatorDigest, ProjectId, UserProfileId};
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace,
    GraphProjectionIdentity, GraphProjectorRevision, NeverCancelled, VerifiedGraphSnapshot,
    take_graph_db_hydration_counters, take_graph_db_traversal_counters,
};
use tracedecay_runtime_core::shard_runtime::VerifiedGraphRuntimePortV1;
use tracedecay_sessions::runtime::git_correlation::{
    CommitEvidence, CommitRelation, CommitRelationFilter, CommitSessionRecord,
    GIT_EVIDENCE_PROJECTOR_REVISION, GitEvidenceGraphHead, GitEvidenceProjectionV1, GitRefFilter,
    GitScopeFilter, SessionGitSpan, SessionsForQuery, SpanOverlapKind, SpanSource,
    git_evidence_projection_identity, open_git_evidence_graph_view,
    publish_git_evidence_projection, recover_git_evidence_projection,
};
use tracedecay_store::{
    FactReadControl, StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1,
    StoreShardIdV1, VerifiedStoreLocatorV1,
};

const BASE_TS: i64 = 1_700_000_000;
const STORE_SIZES: [usize; 2] = [500, 5_000];
const ITERATIONS: usize = 15;

struct MemoryGraphRuntime {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    snapshot: Mutex<Option<VerifiedGraphSnapshot>>,
}

impl MemoryGraphRuntime {
    fn new() -> Self {
        let shard_id = StoreShardIdV1::project(
            BrainId::new("brain.git-evidence-bench").expect("brain id"),
            UserProfileId::new("profile.git-evidence-bench").expect("profile id"),
            ProjectId::new("project.git-evidence-bench").expect("project id"),
        );
        let incarnation = StoreIncarnationV1::new(1).expect("incarnation");
        Self {
            binding: StoreRuntimeBindingV1::new(
                shard_id.clone(),
                incarnation,
                StoreAuthorityEpochV1::new(1).expect("epoch"),
            ),
            locator: VerifiedStoreLocatorV1::new(
                shard_id,
                incarnation,
                LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).expect("digest"),
            ),
            snapshot: Mutex::new(None),
        }
    }
}

impl VerifiedGraphRuntimePortV1 for MemoryGraphRuntime {
    fn relational_binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }

    fn cancel_reconciliation(&self) {}

    fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
        _cancelled: Arc<AtomicBool>,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        let snapshot = VerifiedGraphSnapshot::memory(manifest.clone(), Arc::new(NeverCancelled))?;
        *self.snapshot.lock().expect("snapshot lock") = Some(snapshot.clone());
        Ok(snapshot)
    }

    fn reconcile_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.publish_verified_manifest(manifest, idempotency_key, Arc::new(AtomicBool::new(false)))
    }

    fn verified_snapshot(
        &self,
        projection: &GraphProjectionIdentity,
        _read_control: FactReadControl,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        Ok(self
            .snapshot
            .lock()
            .expect("snapshot lock")
            .as_ref()
            .filter(|snapshot| snapshot.projection() == projection)
            .cloned())
    }
}

fn sha(bucket: usize, commit_in_bucket: usize) -> String {
    format!("{bucket:06x}{commit_in_bucket}{}", "a".repeat(33))
}

/// Two spans and one or two commit records per session: `main` activity for
/// every session plus a per-session feature branch and worktree, with commit
/// records sharing six-digit prefixes.
fn seeded_projection(sessions: usize) -> GitEvidenceProjectionV1 {
    let mut spans = Vec::with_capacity(sessions * 2);
    let mut commits = Vec::with_capacity(sessions);
    for index in 0..sessions {
        let session_id = format!("s{index:05}");
        let provider = if index % 2 == 0 { "codex" } else { "claude" };
        let main_first = BASE_TS + 100 * (index / 3) as i64;
        spans.push(SessionGitSpan {
            span_id: format!("span-main-{index}"),
            provider: provider.to_owned(),
            session_id: session_id.clone(),
            thread_id: None,
            branch: Some("main".to_owned()),
            worktree: "/repo".to_owned(),
            first_ts: main_first,
            last_ts: main_first + 50,
            event_count: 2,
            source: SpanSource::Ingest,
        });
        spans.push(SessionGitSpan {
            span_id: format!("span-feature-{index}"),
            provider: provider.to_owned(),
            session_id: session_id.clone(),
            thread_id: Some(format!("thread-{index}")),
            branch: Some(format!("feature-{}", index % 7)),
            worktree: format!("/wt-{}", index % 3),
            first_ts: BASE_TS + 100 * index as i64 + 10,
            last_ts: BASE_TS + 100 * index as i64 + 40,
            event_count: 1,
            source: SpanSource::HookRoute,
        });
        let produced = index % 4 == 0;
        commits.push(CommitSessionRecord {
            commit_sha: sha(index / 8, (index / 4) % 2),
            provider: provider.to_owned(),
            session_id,
            branch: Some("main".to_owned()),
            worktree: Some("/repo".to_owned()),
            committed_at: BASE_TS + 100 * index as i64,
            span_overlap_kind: if produced {
                SpanOverlapKind::Direct
            } else {
                SpanOverlapKind::WithinSpan
            },
            span_id: Some(format!("span-main-{index}")),
            relation: if produced {
                CommitRelation::Produced
            } else {
                CommitRelation::Observed
            },
            evidence: if produced {
                CommitEvidence::ToolResult
            } else {
                CommitEvidence::TimeOverlap
            },
            confidence: if produced { 100 } else { 20 },
            evidence_message_id: None,
        });
    }
    GitEvidenceProjectionV1::new(format!("bench-{sessions}"), spans, commits)
        .expect("seeded projection is canonical")
}

fn identity() -> GraphProjectionIdentity {
    git_evidence_projection_identity(GraphNamespace::new("project").expect("namespace"))
        .expect("identity")
}

#[derive(Clone, Copy)]
struct RowsTouched {
    property_decodes: u64,
    hydrated_nodes: u64,
    hydrated_edges: u64,
}

fn measure<T>(read: impl Fn() -> T) -> (Duration, RowsTouched, T) {
    take_graph_db_traversal_counters();
    take_graph_db_hydration_counters();
    let mut durations = Vec::with_capacity(ITERATIONS);
    let mut value = None;
    let mut rows = None;
    for _ in 0..ITERATIONS {
        take_graph_db_traversal_counters();
        take_graph_db_hydration_counters();
        let started = Instant::now();
        let observed = read();
        durations.push(started.elapsed());
        let traversal = take_graph_db_traversal_counters();
        let hydration = take_graph_db_hydration_counters();
        rows = Some(RowsTouched {
            property_decodes: traversal.property_decodes,
            hydrated_nodes: hydration.nodes,
            hydrated_edges: hydration.edges,
        });
        value = Some(observed);
    }
    durations.sort();
    (
        durations[durations.len() / 2],
        rows.expect("at least one iteration"),
        value.expect("at least one iteration"),
    )
}

fn report(label: &str, median: Duration, rows: RowsTouched, answer_rows: usize) -> Value {
    json!({
        "path": label,
        "median_micros": median.as_micros(),
        "property_decodes": rows.property_decodes,
        "hydrated_nodes": rows.hydrated_nodes,
        "hydrated_edges": rows.hydrated_edges,
        "answer_rows": answer_rows,
    })
}

fn main() {
    let identity = identity();
    let revision = GraphProjectorRevision::try_from(GIT_EVIDENCE_PROJECTOR_REVISION.to_owned())
        .expect("projector revision");
    let branch_query = SessionsForQuery {
        git_ref: GitRefFilter::Branch("main".to_owned()),
        since: None,
        until: None,
        limit: 20,
    };
    let worktree_query = SessionsForQuery {
        git_ref: GitRefFilter::Worktree("/wt-1".to_owned()),
        since: None,
        until: None,
        limit: 20,
    };
    let commit_query = SessionsForQuery {
        git_ref: GitRefFilter::Commit(sha(7, 1)[..7].to_owned()),
        since: None,
        until: None,
        limit: 20,
    };
    let scope = GitScopeFilter {
        branch: Some("feature-4".to_owned()),
        worktree: Some("/wt-1".to_owned()),
        commit: None,
    };
    let mut configurations = Vec::new();
    for sessions in STORE_SIZES {
        let projection = seeded_projection(sessions);
        let runtime = MemoryGraphRuntime::new();
        publish_git_evidence_projection(
            &runtime,
            identity.clone(),
            &projection,
            &revision,
            GraphIdempotencyKey::new(format!("bench-{sessions}")).expect("idempotency key"),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("publish seeded projection");

        let recover = || {
            recover_git_evidence_projection(&runtime, &identity, Arc::new(AtomicBool::new(false)))
                .expect("full recovery")
                .expect("published head")
        };
        let open =
            || match open_git_evidence_graph_view(&runtime, &identity, Arc::new(NeverCancelled))
                .expect("open graph view")
            {
                GitEvidenceGraphHead::Indexed(view) => view,
                GitEvidenceGraphHead::Unpublished | GitEvidenceGraphHead::Legacy { .. } => {
                    panic!("seeded head must be indexed")
                }
            };

        let mut reads = Vec::new();
        let (median, rows, health) = measure(|| recover().health(None));
        reads.push(report("full_recovery/health", median, rows, 1));
        let (median, rows, view_health) = measure(|| open().health(None));
        reads.push(report("graph_view/health", median, rows, 1));
        assert_eq!(view_health, health);

        for (name, query) in [
            ("sessions_for/branch", &branch_query),
            ("sessions_for/worktree", &worktree_query),
            ("sessions_for/commit_prefix", &commit_query),
        ] {
            let (median, rows, expected) = measure(|| {
                recover().sessions_for_with_relation(query, CommitRelationFilter::Produced)
            });
            reads.push(report(
                &format!("full_recovery/{name}"),
                median,
                rows,
                expected.len(),
            ));
            let (median, rows, observed) = measure(|| {
                open()
                    .sessions_for(query, CommitRelationFilter::Produced)
                    .expect("graph view query")
            });
            reads.push(report(
                &format!("graph_view/{name}"),
                median,
                rows,
                observed.len(),
            ));
            assert_eq!(observed, expected, "{name} must match full recovery");
        }

        let (median, rows, expected) = measure(|| {
            recover()
                .session_ids_for_scope(&scope)
                .expect("non-empty scope")
        });
        reads.push(report("full_recovery/scope", median, rows, expected.len()));
        let (median, rows, observed) = measure(|| {
            open()
                .session_ids_for_scope(&scope)
                .expect("graph view scope")
                .expect("non-empty scope")
        });
        reads.push(report("graph_view/scope", median, rows, observed.len()));
        assert_eq!(observed, expected, "scope must match full recovery");

        configurations.push(json!({
            "sessions": sessions,
            "spans": projection.spans().len(),
            "commit_records": projection.commit_sessions().len(),
            "reads": reads,
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "iterations_per_read": ITERATIONS,
            "configurations": configurations,
        }))
        .expect("serialize report")
    );
}
