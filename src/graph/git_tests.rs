use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use tempfile::TempDir;
use tracedecay_domain::{
    CodeGenerationId, GitGraphEvidenceIntent, GitGraphEvidencePublicationReceipt,
    GitGraphEvidenceTarget, GitOidV1, ProjectId, SessionId, TaskId,
};
use tracedecay_graph_db::{
    GraphDb, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphEntity, GraphFormatVersion,
    GraphMutation, GraphProjectionId, GraphWatermark, GraphWriteBatch, NeverCancelled,
    SourceGeneration,
};

use super::{
    GitEvidenceReceiptSink, GitReferenceRecord, GitTopologyConvergenceOwner, GitTopologyError,
    GitTopologyState, GitTopologyStore, converge_slice,
};

#[derive(Default)]
struct RecordingEvidenceSink {
    receipt: tokio::sync::Mutex<Option<GitGraphEvidencePublicationReceipt>>,
    attempts: tokio::sync::Mutex<Vec<GitGraphEvidencePublicationReceipt>>,
    failures_remaining: AtomicUsize,
    published: tokio::sync::Notify,
}

impl GitEvidenceReceiptSink for RecordingEvidenceSink {
    fn acknowledge<'a>(
        &'a self,
        intent: &'a GitGraphEvidenceIntent,
        receipt: GitGraphEvidencePublicationReceipt,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), GitTopologyError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.attempts.lock().await.push(receipt.clone());
            if receipt.intent_digest() != intent.intent_digest() {
                return Err(GitTopologyError::Contract(
                    "test receipt does not bind its intent".to_owned(),
                ));
            }
            if self
                .failures_remaining
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(GitTopologyError::Repository(
                    "injected acknowledgement failure".to_owned(),
                ));
            }
            *self.receipt.lock().await = Some(receipt);
            self.published.notify_one();
            Ok(())
        })
    }
}

impl RecordingEvidenceSink {
    fn fail_next(&self) {
        self.failures_remaining.store(1, Ordering::Release);
    }

    async fn wait(&self) -> GitGraphEvidencePublicationReceipt {
        loop {
            if let Some(receipt) = self.receipt.lock().await.clone() {
                return receipt;
            }
            self.published.notified().await;
        }
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .output()
        .expect("git fixture command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 git output")
}

fn commit(root: &Path, subject: &str) -> String {
    git(root, &["add", "-A"]);
    git(root, &["commit", "--quiet", "-m", subject]);
    git(root, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn graph_options(path: &Path) -> GraphDbOpenOptions {
    GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path.to_path_buf()),
        expected_format: GraphFormatVersion::new(2).expect("format"),
        durability: GraphDurability::Sync,
        cancellation: Arc::new(NeverCancelled),
    }
}

#[test]
fn reference_point_read_preserves_non_utf8_identity_bytes() {
    let graph_directory = TempDir::new().expect("graph directory");
    let graph = Arc::new(
        GraphDb::open(graph_options(
            &graph_directory.path().join("project.grafeo"),
        ))
        .expect("open graph"),
    );
    let project = ProjectId::new("project.git-reference-bytes").expect("project");
    let reference = GitReferenceRecord {
        name: b"refs/heads/\xfftopic".to_vec(),
        direct_target: None,
        peeled_target: None,
        symbolic_target: Some(b"refs/heads/\xfemain".to_vec()),
    };
    graph
        .apply(
            GraphWriteBatch::new(
                super::namespace(&project).expect("namespace"),
                super::projection().expect("projection"),
                SourceGeneration::new("reference-bytes").expect("generation"),
                GraphWatermark::new("reference-bytes").expect("watermark"),
                vec![GraphMutation::UpsertEntity(
                    super::reference_entity(&reference).expect("reference entity"),
                )],
                Arc::new(NeverCancelled),
            )
            .expect("reference batch"),
        )
        .expect("publish reference");
    let stored = GitTopologyStore::new(graph)
        .reference(&project, reference.name())
        .expect("point read")
        .expect("stored reference");
    assert_eq!(stored, reference);
}

fn converge(project: &ProjectId, root: &Path, graph: &Arc<GraphDb>) -> (usize, usize, u128) {
    let started = Instant::now();
    let mut slices = 0;
    let mut processed = 0;
    loop {
        let outcome =
            converge_slice(project, root, Arc::clone(graph)).expect("convergence slice succeeds");
        slices += 1;
        processed += outcome.processed;
        assert!(outcome.processed <= 256, "slice exceeded commit budget");
        if outcome.done {
            return (slices, processed, started.elapsed().as_micros());
        }
    }
}

#[test]
fn incremental_git_topology_survives_reopen_and_updates_one_commit_and_ref_delete() {
    let repository = TempDir::new().expect("repository");
    git(repository.path(), &["init", "--quiet", "-b", "main"]);
    std::fs::write(repository.path().join("tracked.txt"), "one\n").expect("write fixture");
    let parent = commit(repository.path(), "parent");
    std::fs::write(repository.path().join("tracked.txt"), "two\n").expect("write fixture");
    let child = commit(repository.path(), "child");
    git(repository.path(), &["branch", "release"]);

    let graph_directory = TempDir::new().expect("graph directory");
    let graph_path = graph_directory.path().join("project.grafeo");
    let graph = Arc::new(GraphDb::open(graph_options(&graph_path)).expect("open graph"));
    let project = ProjectId::new("project.git-topology").expect("project id");
    let (cold_slices, cold_commits, cold_micros) = converge(&project, repository.path(), &graph);
    assert_eq!(cold_commits, 2);

    let child_oid = GitOidV1::new(child.clone()).expect("commit oid");
    let parent_oid = GitOidV1::new(parent.clone()).expect("parent oid");
    let store = GitTopologyStore::new(Arc::clone(&graph));
    assert_eq!(
        store
            .parents_of(&project, &child_oid)
            .expect("read parents"),
        vec![parent_oid]
    );
    let release = store
        .reference(&project, b"refs/heads/release")
        .expect("read release ref")
        .expect("release ref");
    assert_eq!(release.direct_target(), Some(&child_oid));
    assert_eq!(release.peeled_target(), Some(&child_oid));

    let evidence = [
        GitGraphEvidenceIntent::new(
            project.clone(),
            child_oid.clone(),
            GitGraphEvidenceTarget::CodeGeneration(
                CodeGenerationId::new("code-generation.git").expect("generation"),
            ),
        )
        .expect("code intent"),
        GitGraphEvidenceIntent::new(
            project.clone(),
            child_oid.clone(),
            GitGraphEvidenceTarget::Session(SessionId::new("session.git").expect("session")),
        )
        .expect("session intent"),
        GitGraphEvidenceIntent::new(
            project.clone(),
            child_oid.clone(),
            GitGraphEvidenceTarget::Work(TaskId::new("task.git").expect("task")),
        )
        .expect("work intent"),
    ];
    graph
        .apply(
            GraphWriteBatch::new(
                super::namespace(&project).expect("namespace"),
                GraphProjectionId::new("evidence-fixture").expect("projection"),
                SourceGeneration::new("evidence-fixture").expect("generation"),
                GraphWatermark::new("evidence-fixture").expect("watermark"),
                evidence
                    .iter()
                    .map(|intent| {
                        GraphEntity::new(
                            super::evidence_target_entity_id(intent.target())
                                .expect("target entity"),
                            Default::default(),
                            Default::default(),
                        )
                        .map(GraphMutation::UpsertEntity)
                        .expect("target")
                    })
                    .collect(),
                Arc::new(NeverCancelled),
            )
            .expect("evidence fixture batch"),
        )
        .expect("publish targets");
    let receipts = store
        .publish_evidence(&project, &evidence)
        .expect("publish evidence");
    assert_eq!(receipts.len(), 3);
    assert!(
        receipts
            .iter()
            .zip(&evidence)
            .all(|(receipt, intent)| receipt.intent_digest() == intent.intent_digest())
    );

    std::fs::write(repository.path().join("tracked.txt"), "three\n").expect("write fixture");
    let newest = commit(repository.path(), "newest");
    let (incremental_slices, incremental_commits, incremental_micros) =
        converge(&project, repository.path(), &graph);
    assert_eq!(incremental_slices, 1);
    assert_eq!(
        incremental_commits, 1,
        "one-commit advance must not replay history"
    );
    assert_eq!(
        store
            .parents_of(&project, &GitOidV1::new(newest).expect("new commit"))
            .expect("updated topology"),
        vec![child_oid.clone()]
    );

    git(repository.path(), &["branch", "-f", "release", &parent]);
    let (force_slices, force_commits, force_micros) = converge(&project, repository.path(), &graph);
    assert_eq!(force_slices, 1);
    assert_eq!(force_commits, 0, "force-move to known history is refs-only");
    assert_eq!(
        store
            .reference(&project, b"refs/heads/release")
            .expect("read force-moved release")
            .expect("release ref")
            .direct_target(),
        Some(&parent_oid)
    );

    git(repository.path(), &["branch", "-D", "release"]);
    let (delete_slices, delete_commits, delete_micros) =
        converge(&project, repository.path(), &graph);
    assert_eq!(delete_slices, 1);
    assert_eq!(delete_commits, 0, "ref deletion must not replay history");
    assert!(
        store
            .reference(&project, b"refs/heads/release")
            .expect("read deleted release ref")
            .is_none()
    );

    drop(store);
    let graph = Arc::try_unwrap(graph).expect("sole graph owner");
    graph.close().expect("close graph");
    let reopened = Arc::new(GraphDb::open(graph_options(&graph_path)).expect("reopen graph"));
    let reopened_store = GitTopologyStore::new(reopened);
    assert_eq!(
        reopened_store
            .evidence_for(&project, &child_oid)
            .expect("reopened topology")
            .len(),
        3
    );
    eprintln!(
        "git convergence timings: cold={cold_micros}us/{cold_slices} slices, \
         one_commit={incremental_micros}us, force_move={force_micros}us, \
         ref_delete={delete_micros}us"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_wakes_coalesce_to_one_job_and_shutdown_joins_owner() {
    let repository = TempDir::new().expect("repository");
    git(repository.path(), &["init", "--quiet", "-b", "main"]);
    std::fs::write(repository.path().join("tracked.txt"), "one\n").expect("write fixture");
    commit(repository.path(), "initial");
    for index in 0..300 {
        git(
            repository.path(),
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                &format!("cold-{index}"),
            ],
        );
    }

    let graph_directory = TempDir::new().expect("graph directory");
    let graph_path = graph_directory.path().join("project.grafeo");
    let graph = Arc::new(GraphDb::open(graph_options(&graph_path)).expect("open graph"));
    let project = ProjectId::new("project.git-owner").expect("project id");
    let canonical_root = repository.path().canonicalize().expect("canonical root");
    let session = SessionId::new("session.git-owner").expect("session");
    let session_entity =
        tracedecay_global_db::session_temporal::relations::session_entity_id(&session)
            .expect("session entity");
    graph
        .apply(
            GraphWriteBatch::new(
                super::namespace(&project).expect("namespace"),
                GraphProjectionId::new("session-evidence-fixture").expect("projection"),
                SourceGeneration::new("session-evidence-fixture").expect("generation"),
                GraphWatermark::new("session-evidence-fixture").expect("watermark"),
                vec![
                    GraphEntity::new(session_entity, Default::default(), Default::default())
                        .map(GraphMutation::UpsertEntity)
                        .expect("session target"),
                ],
                Arc::new(NeverCancelled),
            )
            .expect("session target batch"),
        )
        .expect("publish session target");
    let owner = GitTopologyConvergenceOwner::start(project.clone(), Arc::clone(&graph));
    let evidence_commit = GitOidV1::new(
        git(repository.path(), &["rev-parse", "HEAD"])
            .trim()
            .to_owned(),
    )
    .expect("evidence commit");
    let evidence = GitGraphEvidenceIntent::new(
        project.clone(),
        evidence_commit.clone(),
        GitGraphEvidenceTarget::Session(session.clone()),
    )
    .expect("session evidence");
    let evidence_sink = Arc::new(RecordingEvidenceSink::default());
    evidence_sink.fail_next();
    let evidence_sink_port: Arc<dyn GitEvidenceReceiptSink> = evidence_sink.clone();
    owner
        .enqueue_evidence(&canonical_root, evidence, evidence_sink_port)
        .await
        .expect("enqueue evidence");
    let barrier = Arc::new(tokio::sync::Barrier::new(101));
    let mut wakes = tokio::task::JoinSet::new();
    for _ in 0..100 {
        let owner = Arc::clone(&owner);
        let barrier = Arc::clone(&barrier);
        let root = canonical_root.clone();
        wakes.spawn(async move {
            barrier.wait().await;
            owner.wake(&root).await.expect("wake owner");
        });
    }
    barrier.wait().await;
    while let Some(result) = wakes.join_next().await {
        result.expect("wake task");
    }

    let store = GitTopologyStore::new(Arc::clone(&graph));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if store.freshness(&project).expect("freshness").state == GitTopologyState::Complete {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "convergence did not complete"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(owner.started_jobs(), 1, "same-root wakes must coalesce");
    assert_eq!(
        owner.opened_repositories(),
        1,
        "one repository and object cache must survive every bounded slice"
    );
    assert!(
        owner.completed_slices() >= 2,
        "the retained repository must cover multiple cooperative slices"
    );
    let receipt = tokio::time::timeout(std::time::Duration::from_secs(5), evidence_sink.wait())
        .await
        .expect("evidence acknowledgement deadline");
    let attempts = evidence_sink.attempts.lock().await;
    assert_eq!(attempts.as_slice(), &[receipt.clone(), receipt]);
    drop(attempts);
    assert_eq!(
        store
            .evidence_for(&project, &evidence_commit)
            .expect("published owner evidence"),
        vec![GitGraphEvidenceTarget::Session(session)]
    );

    for index in 0..300 {
        git(
            repository.path(),
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                &format!("queued-{index}"),
            ],
        );
    }
    owner.wake(&canonical_root).await.expect("wake owner");
    while owner.started_jobs() < 2 {
        tokio::task::yield_now().await;
    }
    owner.shutdown().await.expect("shutdown owner");
    assert!(owner.is_shutdown().await, "owner task must be joined");
    assert_eq!(
        store.freshness(&project).expect("partial freshness").state,
        GitTopologyState::Partial,
        "shutdown must publish preemption before the graph closes"
    );
    drop(store);
    drop(owner);
    let graph = Arc::try_unwrap(graph).expect("sole graph owner");
    graph.close().expect("close interrupted graph");
    let reopened = Arc::new(GraphDb::open(graph_options(&graph_path)).expect("reopen graph"));
    let (resume_slices, resumed_commits, resume_micros) =
        converge(&project, repository.path(), &reopened);
    assert!(
        resumed_commits <= 300,
        "restart replayed already-published history"
    );
    assert_eq!(
        GitTopologyStore::new(reopened)
            .freshness(&project)
            .expect("resumed freshness")
            .state,
        GitTopologyState::Complete
    );
    eprintln!(
        "git owner timings: resumed={resume_micros}us/{resume_slices} slices, \
         resumed_commits={resumed_commits}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_repository_is_a_background_failed_state() {
    let not_repository = TempDir::new().expect("non-repository");
    let graph_directory = TempDir::new().expect("graph directory");
    let graph_path = graph_directory.path().join("project.grafeo");
    let graph = Arc::new(GraphDb::open(graph_options(&graph_path)).expect("open graph"));
    let project = ProjectId::new("project.git-unavailable").expect("project id");
    let owner = GitTopologyConvergenceOwner::start(project.clone(), Arc::clone(&graph));
    owner
        .wake(
            &not_repository
                .path()
                .canonicalize()
                .expect("canonical root"),
        )
        .await
        .expect("enqueue background work");

    let store = GitTopologyStore::new(graph);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if store.freshness(&project).expect("freshness").state == GitTopologyState::Failed {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "background failure was not published"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        store
            .freshness(&project)
            .expect("failed freshness")
            .reason
            .is_some_and(|reason| reason.contains("not a Git repository"))
    );
    owner.shutdown().await.expect("shutdown owner");
    assert!(owner.is_shutdown().await, "owner task must be joined");
}
