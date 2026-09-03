//! One-shot, cross-process barrier at a selected authoritative boundary.
//!
//! The daemon-crash harness needs the daemon to stop *inside* a chosen
//! observation-persistence boundary so the test can kill it there and observe
//! what the boundary guaranteed. Both boundaries live in different crates —
//! the pre-commit one inside the rusqlite write executor, the pre-ack one in
//! the store adapter that owns the client response — so the claim protocol
//! lives here, where both can reach it.
//!
//! The whole module is compiled only under
//! `--cfg tracedecay_observation_fault_harness`, and it sits outside
//! `src/` because the barrier needs the filesystem and thread authority that
//! the store contracts in that tree are forbidden to hold.

use std::path::PathBuf;
use std::time::{Duration, Instant};

const BARRIER_DIR_ENV: &str = "TRACEDECAY_TEST_OBSERVATION_PERSIST_BARRIER_DIR";
const RELEASE_TIMEOUT: Duration = Duration::from_secs(10);
const RELEASE_POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObservationPersistBarrierStageV1 {
    PostWritePreCommit,
    PostCommitPreAck,
}

impl ObservationPersistBarrierStageV1 {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PostWritePreCommit => "post-write-pre-commit",
            Self::PostCommitPreAck => "post-commit-pre-ack",
        }
    }
}

/// Blocks at `stage` for `session_id` when the harness armed exactly that pair.
///
/// The caller is held inside the boundary it just reached, so this blocks the
/// current thread rather than yielding: a pre-commit waiter must keep its
/// transaction open, and a pre-ack waiter must keep the client response
/// unsent. The wait is bounded so a failed test cannot strand a live daemon.
///
/// Returns the operation label and detail of any filesystem failure; callers
/// map that into their own store error type.
pub fn wait_at_observation_persist_barrier(
    stage: ObservationPersistBarrierStageV1,
    session_id: &str,
) -> Result<(), (&'static str, String)> {
    let Some(root) = std::env::var_os(BARRIER_DIR_ENV) else {
        return Ok(());
    };
    let root = PathBuf::from(root);
    let armed = root.join("armed");
    let expected = match std::fs::read_to_string(&armed) {
        Ok(expected) => expected,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(("read observation test barrier", error.to_string())),
    };
    let Some((expected_stage, expected_session)) = expected.split_once('\n') else {
        return Err((
            "read observation test barrier",
            "armed barrier must contain a stage and session identifier".to_owned(),
        ));
    };
    if expected_stage.trim() != stage.as_str() || expected_session.trim() != session_id {
        return Ok(());
    }
    // Renaming is the claim: a concurrent ingest of the same session cannot
    // also consume this one-shot barrier and let the test's own request run
    // through unblocked.
    match std::fs::rename(&armed, root.join("claimed")) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(("claim observation test barrier", error.to_string())),
    }
    std::fs::write(root.join("arrived"), b"arrived\n").map_err(|error| {
        (
            "publish observation test barrier arrival",
            error.to_string(),
        )
    })?;

    let release = root.join("release");
    let deadline = Instant::now() + RELEASE_TIMEOUT;
    loop {
        match release.try_exists() {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => {
                return Err(("read observation test barrier release", error.to_string()));
            }
        }
        if Instant::now() >= deadline {
            return Err((
                "wait at observation test barrier",
                "timed out waiting for release".to_owned(),
            ));
        }
        std::thread::sleep(RELEASE_POLL);
    }
}
