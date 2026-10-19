use tracedecay_domain::HostIntegrationIdV1;
use tracedecay_host_admission::{
    HostAdmissionBroker, HostAdmissionRuntime, SharedHostAdmissionBroker, SpoolBounds,
};
use tracedecay_mcp::hook_events::{HookEventKind, encode_durable_hook_event_plan};

use super::*;
use crate::mcp::server::writer_test_support::init_indexed_repo;
use crate::test_support::git::git;

async fn reader(
    broker: &SharedHostAdmissionBroker,
    root: &Path,
    scope: &ResolvedScope,
) -> HookSourcePending {
    let slot = Arc::new(HostAdmissionBrokerSlot::default());
    slot.get_or_try_init(|| async { Ok(Arc::clone(broker)) })
        .await
        .unwrap();
    HookSourcePending {
        broker: slot,
        root: root.to_owned(),
        scope: scope.clone(),
    }
}

fn edit(root: &Path, scope: ResolvedScope) -> Vec<u8> {
    encode_durable_hook_event_plan(&HookEventPlan::SyncFilesAt {
        root: root.to_owned(),
        scope,
        agent: HostIntegrationIdV1::Codex,
        event_kind: HookEventKind::FileEdit,
        had_command: false,
        route: None,
        rel_paths: vec!["src/a.rs".to_owned()],
    })
    .unwrap()
}

fn broker(path: &Path) -> SharedHostAdmissionBroker {
    let (runtime, _) = HostAdmissionRuntime::open(path, SpoolBounds::default()).unwrap();
    Arc::new(HostAdmissionBroker::new(runtime))
}

#[tokio::test]
async fn pending_source_is_exactly_routed_across_linked_worktrees_and_branch_changes() {
    let (graph, project, _authority) = init_indexed_repo().await;
    let linked_parent = tempfile::tempdir().unwrap();
    let linked = linked_parent.path().join("linked");
    git(
        project.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    let project_id = graph
        .store_layout()
        .identity
        .project_id
        .as_ref()
        .unwrap()
        .clone()
        .try_into()
        .unwrap();
    let main_scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(project.path(), &project_id)
            .unwrap();
    let linked_scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(&linked, &project_id).unwrap();
    let spool = tempfile::tempdir().unwrap();
    let broker = broker(spool.path());
    let main = reader(&broker, project.path(), &main_scope).await;
    let other = reader(&broker, &linked, &linked_scope).await;
    broker
        .admit("linked", &edit(&linked, linked_scope))
        .await
        .unwrap();
    assert!(!main.read_pending().unwrap());
    assert!(other.read_pending().unwrap());
    git(project.path(), &["checkout", "-q", "-b", "next"]);
    let current =
        tracedecay_code_index_runtime::resolved_scope_for_project(project.path(), &project_id)
            .unwrap();
    assert_ne!(current.reference, main_scope.reference);
    broker
        .admit("main", &edit(project.path(), main_scope))
        .await
        .unwrap();
    assert!(
        reader(&broker, project.path(), &current)
            .await
            .read_pending()
            .unwrap()
    );
    let replay = broker.begin_replay().await.unwrap();
    let first = replay.lease_next().await.unwrap().unwrap();
    let second = replay.lease_next().await.unwrap().unwrap();
    assert!(
        main.read_pending().unwrap(),
        "leased debt still fences source currency"
    );
    replay.commit(second.seq).await.unwrap();
    assert!(
        !main.read_pending().unwrap(),
        "completed debt behind a prefix is settled"
    );
    assert!(other.read_pending().unwrap());
    replay.commit(first.seq).await.unwrap();
    assert!(!other.read_pending().unwrap());
}

#[tokio::test]
async fn unscoped_legacy_edits_remain_pending_and_invalid_plans_fail_closed() {
    let (graph, project, _authority) = init_indexed_repo().await;
    let project_id = graph
        .store_layout()
        .identity
        .project_id
        .as_ref()
        .unwrap()
        .clone()
        .try_into()
        .unwrap();
    let scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(project.path(), &project_id)
            .unwrap();
    for (payload, expected) in [
        (
            encode_durable_hook_event_plan(&HookEventPlan::SyncFiles(vec!["src/a.rs".to_owned()]))
                .unwrap(),
            Ok(true),
        ),
        (
            encode_durable_hook_event_plan(&HookEventPlan::Noop).unwrap(),
            Ok(false),
        ),
        (
            b"invalid".to_vec(),
            Err(CodeIndexFreshnessReadFailureV1::ReadFailed),
        ),
        (
            br#"{"version":999,"plan":{"kind":"noop"}}"#.to_vec(),
            Err(CodeIndexFreshnessReadFailureV1::ReadFailed),
        ),
    ] {
        let spool = tempfile::tempdir().unwrap();
        let broker = broker(spool.path());
        broker.admit("source", &payload).await.unwrap();
        assert_eq!(
            reader(&broker, project.path(), &scope).await.read_pending(),
            expected
        );
    }
    let mut corrupt: serde_json::Value =
        serde_json::from_slice(&edit(project.path(), scope.clone())).unwrap();
    corrupt["plan"]["scope"]["scope_digest"] =
        serde_json::json!(format!("sha256:{}", "0".repeat(64)));
    let spool = tempfile::tempdir().unwrap();
    let broker = broker(spool.path());
    broker
        .admit("source", &serde_json::to_vec(&corrupt).unwrap())
        .await
        .unwrap();
    assert_eq!(
        reader(&broker, project.path(), &scope).await.read_pending(),
        Err(CodeIndexFreshnessReadFailureV1::ReadFailed)
    );
}
