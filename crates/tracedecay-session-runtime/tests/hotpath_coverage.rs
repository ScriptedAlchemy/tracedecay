//! Unguarded hotpath contract for `tracedecay-session-runtime`.
//!
//! With either feature configuration, setting report environment variables
//! alone must not create a report without a process-boundary guard.

use tracedecay_session_memory::context::{
    ProfileId, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
};
use tracedecay_session_memory::session::lcm::{LcmAuthorityRequest, LcmStatusQuery};
use tracedecay_session_runtime::lcm_authority::mount_registered_lcm_authority;

/// Deterministic, daemon-free workload that reaches this crate's measured
/// sites: `daemon.lcm.mount.execute`, `daemon.lcm.execute`, and
/// `daemon.lcm.status`. Registered-database fixtures come from the global-db
/// test harness; no daemon or socket is involved.
async fn run_mounted_lcm_status_workload() -> usize {
    let directory = tempfile::tempdir().expect("create registered db fixture dir");
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
        .expect("mount registered lcm authority for owning profile identity");

    let first = mounted
        .execute(LcmAuthorityRequest::Status(LcmStatusQuery {
            provider: "claude".to_owned(),
            session_id: Some("session.hotpath-coverage.first".to_owned()),
            deep: false,
        }))
        .await
        .expect("mounted status must be invocable");
    let second = mounted
        .execute(LcmAuthorityRequest::Status(LcmStatusQuery {
            provider: "claude".to_owned(),
            session_id: Some("session.hotpath-coverage.second".to_owned()),
            deep: false,
        }))
        .await
        .expect("mounted status must be invocable");
    assert_ne!(
        first.receipt.grant_digest, second.receipt.grant_digest,
        "each mounted request must mint its own grant digest"
    );

    2
}

fn block_on_workload() -> usize {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime")
        .block_on(run_mounted_lcm_status_workload())
}

mod unguarded {
    use std::path::Path;

    /// The workload behaves identically and report environment is ignored
    /// until a process-boundary guard is installed.
    #[test]
    fn workload_is_a_no_op_for_profiling() {
        let report =
            Path::new(env!("CARGO_TARGET_TMPDIR")).join("session-runtime-hotpath-off.json");
        let _ = std::fs::remove_file(&report);
        // SAFETY: single-threaded with respect to readers — the feature-off
        // build contains no hotpath runtime and nothing else in this test
        // binary reads these variables.
        unsafe {
            std::env::set_var("HOTPATH_OUTPUT_FORMAT", "json");
            std::env::set_var("HOTPATH_OUTPUT_PATH", &report);
        }

        assert!(super::block_on_workload() > 0);

        assert!(
            !report.exists(),
            "unguarded workload must never write a hotpath report"
        );
    }
}
