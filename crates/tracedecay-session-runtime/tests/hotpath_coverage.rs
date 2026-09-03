//! Hotpath coverage contract for `tracedecay-session-runtime`.
//!
//! Feature-off (default build): every hotpath macro must be a no-op — no
//! metrics listener on 6770/6771, no report file even when the report
//! environment is set, and `hotpath` must stay out of the crate's default
//! features.
//!
//! Feature-on (`--features hotpath`): a process-boundary guard must capture
//! this crate's measured mounted-LCM authority path in the report, proving
//! the instrumentation is real rather than dead configuration. The measured
//! sites here are `future = true` spans, so the report must include the
//! futures section as well as functions timing.

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

/// Collects the entries of one feature array (for example `default`) from
/// this crate's manifest, tolerating multi-line arrays. Returns `None` when
/// the feature is not declared at all.
fn manifest_feature_array(manifest: &str, feature: &str) -> Option<String> {
    let mut in_features = false;
    let mut collecting = false;
    let mut collected = String::new();
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_features = trimmed == "[features]";
            continue;
        }
        if collecting {
            collected.push_str(trimmed);
            if trimmed.contains(']') {
                return Some(collected);
            }
            continue;
        }
        if in_features
            && let Some(rest) = trimmed.strip_prefix(feature)
            && let Some(array) = rest.trim_start().strip_prefix('=')
        {
            collected.push_str(array.trim());
            if collected.contains(']') {
                return Some(collected);
            }
            collecting = true;
        }
    }
    None
}

/// The profiling features must remain opt-in: neither `default` nor any
/// production-shaped feature set of this crate may pull in hotpath.
#[test]
fn hotpath_stays_out_of_default_and_production_features() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read crate manifest");
    for gate in ["default", "production"] {
        if let Some(entries) = manifest_feature_array(&manifest, gate) {
            assert!(
                !entries.contains("hotpath"),
                "feature `{gate}` must never enable hotpath, found: {entries}"
            );
        }
    }
}

#[cfg(not(feature = "hotpath"))]
mod feature_off {
    use std::net::TcpStream;
    use std::path::Path;

    /// With the feature off the macros expand to their primary expression:
    /// the workload behaves identically, the report environment is ignored,
    /// and no metrics listener appears.
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
            "feature-off build must never write a hotpath report"
        );
        for port in [6770u16, 6771] {
            assert!(
                TcpStream::connect(("127.0.0.1", port)).is_err(),
                "feature-off build must not expose a hotpath listener on port {port} \
                 (a listener here means another process on this machine is serving it)"
            );
        }
    }
}

#[cfg(feature = "hotpath")]
mod feature_on {
    use std::path::Path;

    /// A guard-scoped run of the same workload must record this crate's
    /// measured sites, proving `--features hotpath` produces live
    /// instrumentation and not an empty report.
    #[test]
    fn guard_report_captures_measured_lcm_authority_sites() {
        // SAFETY: set before the first guard build in this process, which is
        // the only reader; the metrics listener must stay off in tests.
        unsafe { std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1") };
        let report = Path::new(env!("CARGO_TARGET_TMPDIR")).join("session-runtime-hotpath-on.json");
        let _ = std::fs::remove_file(&report);

        {
            let _guard = hotpath::HotpathGuardBuilder::new("session-runtime-hotpath-coverage")
                .format(hotpath::Format::Json)
                .output_path(&report)
                .report("functions-timing,futures")
                .build();
            assert!(super::block_on_workload() > 0);
        }

        let report_text =
            std::fs::read_to_string(&report).expect("feature-on guard drop must write a report");
        for label in [
            "daemon.lcm.mount.execute",
            "daemon.lcm.execute",
            "daemon.lcm.status",
        ] {
            assert!(
                report_text.contains(label),
                "hotpath report must capture measured site `{label}`: {report_text}"
            );
        }
    }
}
