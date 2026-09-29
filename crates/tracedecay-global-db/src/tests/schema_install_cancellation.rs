//! Daemon shutdown cancelling a registered store attach mid schema install.

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::task::Poll;

use tempfile::TempDir;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::db::{
    DaemonDatabaseScope, Database, DatabaseAuthority, RegisteredTestRuntimeFixtureV1,
    TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
};

use super::harness::TEST_RUNTIME_NONCE;
use crate::RegisteredGlobalDbOwnerV1;

/// An existing profile-sessions store file with no schema objects, the shape
/// daemon attach installs the full registered schema into.
struct EmptyStore {
    path: PathBuf,
    _scope: DaemonDatabaseScope,
    _directory: TempDir,
}

impl EmptyStore {
    fn create() -> Self {
        crate::register_registered_schema_installer();
        let directory = tempfile::tempdir().unwrap();
        let profile_root = directory.path().join("profile");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile_root).unwrap();
        let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &profile_root,
            TEST_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed),
            "schema install cancellation",
        )
        .unwrap();
        let path = tracedecay_sessions::runtime::user_sessions_db_path(&profile_root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::File::create(&path).unwrap();
        Self {
            path,
            _scope: scope,
            _directory: directory,
        }
    }

    async fn publish(&self) -> RegisteredTestRuntimeFixtureV1 {
        let authority =
            DatabaseAuthority::for_owned_runtime(&self.path, "schema install cancellation test")
                .unwrap();
        Database::publish_registered_daemon_test_runtime_with_retirement_control(
            &self.path,
            &authority,
            TestDatabaseRuntimeMode::Existing,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap()
    }

    async fn schema_objects(&self) -> Vec<String> {
        let (owner, _runtime, _retirement) = self.publish().await.into_parts();
        let database = owner.issue_lease().unwrap();
        let mut rows = database
            .read_connection()
            .query(
                "SELECT type || ':' || name FROM sqlite_schema
                 WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
                (),
            )
            .await
            .unwrap();
        let mut objects = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            objects.push(row.get::<String>(0).unwrap());
        }
        objects
    }

    /// Runs daemon admission, cancelling its token just before the
    /// `cancel_at`-th poll. Every poll after the first follows one completed
    /// statement round trip, so sweeping `cancel_at` lands the cancellation at
    /// each statement boundary of the install. Returns the outcome and how
    /// many polls admission took.
    async fn attach_cancelled_at(&self, cancel_at: usize) -> (Result<(), TraceDecayError>, usize) {
        let (owner, _runtime, _retirement) = self.publish().await.into_parts();
        let cancellation = CancellationToken::new();
        let mut admission = Box::pin(RegisteredGlobalDbOwnerV1::admit_and_attach_for_daemon(
            owner,
            &cancellation,
        ));
        let mut polls = 0;
        let outcome = std::future::poll_fn(|context| {
            polls += 1;
            if polls == cancel_at {
                cancellation.cancel();
            }
            match admission.as_mut().poll(context) {
                Poll::Ready(result) => Poll::Ready(result.map(drop)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;
        (outcome, polls)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_daemon_attach_leaves_the_store_empty_or_fully_installed_and_reopens() {
    let reference = EmptyStore::create();
    let (outcome, total_polls) = reference.attach_cancelled_at(usize::MAX).await;
    outcome.unwrap();
    let installed = reference.schema_objects().await;
    assert!(
        installed.contains(&"table:code_projects".to_owned()),
        "a completed attach installs the registered schema: {installed:?}"
    );

    let stride = (total_polls / 20).max(1);
    let mut cancelled = Vec::new();
    let mut completed = Vec::new();
    for cancel_at in (1..total_polls).step_by(stride).chain([total_polls + 10]) {
        let store = EmptyStore::create();
        let (outcome, _) = store.attach_cancelled_at(cancel_at).await;
        let objects = store.schema_objects().await;
        match outcome {
            Err(error) => {
                assert!(
                    error.is_store_open_cancelled(),
                    "cancel at poll {cancel_at}/{total_polls} must fail typed: {error:?}"
                );
                assert_eq!(
                    objects,
                    Vec::<String>::new(),
                    "cancel at poll {cancel_at}/{total_polls} must roll the install back"
                );
                cancelled.push(cancel_at);
            }
            Ok(()) => {
                assert_eq!(
                    objects, installed,
                    "an attach that commits before observing cancel at poll {cancel_at}/{total_polls} installs everything"
                );
                completed.push(cancel_at);
            }
        }
        let (reopened, _) = store.attach_cancelled_at(usize::MAX).await;
        reopened.unwrap_or_else(|error| {
            panic!("reopen after cancel at poll {cancel_at}/{total_polls} failed: {error:?}")
        });
        assert_eq!(store.schema_objects().await, installed);
    }
    // Only the commit and the idempotent post-commit tail may run past a
    // cancel: every sampled point up to the commit boundary rolls back.
    let last_cancelled = cancelled.iter().max().copied().unwrap_or(0);
    let first_completed = completed.iter().min().copied().unwrap_or(usize::MAX);
    assert!(
        cancelled.contains(&1)
            && last_cancelled >= total_polls / 2
            && first_completed.saturating_sub(last_cancelled) <= 2 * stride,
        "cancellation must stop the install at every statement before its commit; \
         cancelled at {cancelled:?}, completed at {completed:?} of {total_polls} polls"
    );
    assert!(completed.contains(&(total_polls + 10)));
}
