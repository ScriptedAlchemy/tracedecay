//! Runtime coverage for the mounted LCM authority's production labels.

use tracedecay_session_memory::context::{
    ProfileId, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
};
use tracedecay_session_memory::session::lcm::{LcmAuthorityRequest, LcmStatusQuery};
use tracedecay_session_runtime::lcm_authority::mount_registered_lcm_authority;

#[cfg(feature = "hotpath")]
#[path = "../../../tests/hotpath_report_support.rs"]
mod hotpath_report_support;

#[cfg(feature = "hotpath")]
const EXPECTED_LABELS: &[&str] = &[
    "daemon.lcm.mount.execute",
    "daemon.lcm.execute",
    "daemon.lcm.status",
];

async fn exercise_mounted_lcm_authority() {
    let directory = tempfile::tempdir().expect("create registered database fixture");
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::profile(
        directory.path(),
    )
    .await
    .expect("open registered profile database");
    let database = runtime.profile_database_arc();
    let shard = database.binding().shard_id.clone();
    let identity = ResolvedSessionIdentity::for_profile(
        ProfileId::new(shard.profile_id.as_str()).expect("valid profile id"),
        SessionStoreId::new("store.profile.hotpath-coverage").expect("valid store id"),
        SessionRootId::new("root.profile.hotpath-coverage").expect("valid root id"),
    );
    let mounted = mount_registered_lcm_authority(database, identity, &shard)
        .expect("mount registered LCM authority");

    let first = mounted
        .execute(LcmAuthorityRequest::Status(LcmStatusQuery {
            provider: "claude".to_owned(),
            session_id: Some("session.hotpath-coverage.first".to_owned()),
            deep: false,
        }))
        .await
        .expect("execute first mounted status");
    let second = mounted
        .execute(LcmAuthorityRequest::Status(LcmStatusQuery {
            provider: "claude".to_owned(),
            session_id: Some("session.hotpath-coverage.second".to_owned()),
            deep: false,
        }))
        .await
        .expect("execute second mounted status");
    assert_ne!(first.receipt.grant_digest, second.receipt.grant_digest);
}

fn block_on_workload() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime")
        .block_on(exercise_mounted_lcm_authority());
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn measured_mounted_lcm_authority_runs_with_hotpath_off() {
    block_on_workload();
}

#[cfg(feature = "hotpath")]
#[test]
fn measured_mounted_lcm_authority_emits_exact_labels() {
    hotpath_report_support::assert_hotpath_report(
        "session-runtime-hotpath-coverage",
        "functions-timing,futures",
        EXPECTED_LABELS,
        block_on_workload,
    );
}
