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

/// The operation a cancel reports once it rolled back an open schema
/// transaction, rather than stopping before the transaction began.
const INSTALL_ROLLBACK: &str = "roll back attached registered global schema";

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
    /// `cancel_at`-th poll, so sweeping `cancel_at` samples cancellation
    /// points across admission. Returns the outcome and how many polls
    /// admission took.
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

#[asupersync::test(flavor = "multi_thread")]
async fn cancelled_daemon_attach_rolls_back_schema_and_reopens() {
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
    let mut rolled_back_installs = Vec::new();
    let mut completed = Vec::new();
    for cancel_at in (1..total_polls).step_by(stride).chain([total_polls + 10]) {
        let store = EmptyStore::create();
        // Publishing the runtime installs its writer ledger before admission.
        // Cancellation must preserve exactly that pre-admission schema.
        let before_admission = store.schema_objects().await;
        let (outcome, _) = store.attach_cancelled_at(cancel_at).await;
        let objects = store.schema_objects().await;
        match outcome {
            Err(error) => {
                assert!(
                    error.is_store_open_cancelled(),
                    "cancel at poll {cancel_at}/{total_polls} must fail typed: {error:?}"
                );
                assert_eq!(
                    objects, before_admission,
                    "cancel at poll {cancel_at}/{total_polls} must roll the install back"
                );
                if error
                    .project_route_context()
                    .is_some_and(|(_, _, detail)| detail.starts_with(INSTALL_ROLLBACK))
                {
                    rolled_back_installs.push(cancel_at);
                }
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
    assert!(
        cancelled.contains(&1),
        "a cancel before admission starts must stop it; cancelled at {cancelled:?}"
    );
    assert!(
        !rolled_back_installs.is_empty(),
        "a cancel inside the schema transaction must roll the install back; \
         cancelled at {cancelled:?}, completed at {completed:?} of {total_polls} polls"
    );
    assert!(completed.contains(&(total_polls + 10)));
}
