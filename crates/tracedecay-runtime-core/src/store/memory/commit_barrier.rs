//! One-shot, cross-process barrier at the durable fact-commit boundary.
//!
//! The retained memory owners answer a budget that expires *after* the commit
//! point with a `PartialEffect` carrying a committed receipt and a
//! Reconcile-only legal action. That terminal is the whole reason the budget
//! exists, and until now nothing could induce it end-to-end: from outside the
//! daemon the window between `try_begin_commit()` and settlement is one fsync
//! wide, so a wall-clock deadline either lands before the commit (a typed
//! timeout, nothing written) or after settlement (a plain success). Measuring
//! that window on a real daemon put it under a tenth of a second inside a
//! multi-second request — a race, not a test.
//!
//! This barrier removes the race without inventing an outcome: the daemon does
//! the real commit, then parks *inside* the operation until the test releases
//! it. The caller's real deadline elapses while it is parked, so the owner's
//! own settlement classification observes exactly what production observes when
//! a slow commit outlives its budget — commit started, deadline elapsed — and
//! produces the real `PartialEffect`.
//!
//! Compiled only under the `test-transport` feature, the same gate the rest of
//! this crate's daemon-side fixtures use. It is a sibling of the observation
//! persistence barrier in `tracedecay-store/test-support/fault_harness.rs`,
//! which claims its arming file the same way; this one lives here because the
//! boundary it guards is this crate's memory write path, and it parks
//! asynchronously because it holds no open transaction.

use std::path::PathBuf;
use std::time::{Duration, Instant};

const BARRIER_DIR_ENV: &str = "TRACEDECAY_TEST_FACT_COMMIT_BARRIER_DIR";
const RELEASE_TIMEOUT: Duration = Duration::from_secs(120);
const RELEASE_POLL: Duration = Duration::from_millis(10);

/// Parks after a durable fact commit when the harness armed the barrier.
///
/// A no-op unless [`BARRIER_DIR_ENV`] names a directory holding an `armed`
/// file. Renaming `armed` to `claimed` is the claim: a concurrent fact commit
/// cannot also consume this one-shot barrier, so exactly the mutation the test
/// is watching is the one that parks. Arrival is published as `arrived`; the
/// park ends when `release` appears, or when the bounded wait expires so a
/// failed test cannot strand a live daemon.
pub(super) async fn wait_after_durable_fact_commit() {
    let Some(root) = std::env::var_os(BARRIER_DIR_ENV) else {
        return;
    };
    let root = PathBuf::from(root);
    let armed = root.join("armed");
    if !matches!(armed.try_exists(), Ok(true)) {
        return;
    }
    if std::fs::rename(&armed, root.join("claimed")).is_err() {
        return;
    }
    if std::fs::write(root.join("arrived"), b"arrived\n").is_err() {
        return;
    }
    let release = root.join("release");
    let deadline = Instant::now() + RELEASE_TIMEOUT;
    while Instant::now() < deadline {
        if matches!(release.try_exists(), Ok(true)) {
            return;
        }
        tokio::time::sleep(RELEASE_POLL).await;
    }
}
