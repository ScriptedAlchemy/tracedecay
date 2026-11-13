//! Cancellation of registered daemon admission after prospective schema publication.

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::task::Poll;

use tempfile::TempDir;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::db::{
    DaemonDatabaseScope, Database, DatabaseAuthority, RegisteredTestRuntimeFixtureV1,
    RegisteredTestRuntimeRetirementControlV1, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
};
use tracedecay_runtime_core::shard_runtime::registry::{
    StoreRuntimeRetirementOutcome, StoreRuntimeRetirementResult,
};

use super::harness::TEST_RUNTIME_NONCE;
use crate::RegisteredGlobalDbOwnerV1;

async fn retire_runtime(retirement: RegisteredTestRuntimeRetirementControlV1) {
    let mut reservation = match retirement
        .registry()
        .reserve_retirement_batch(vec![retirement.retirement_target()])
    {
        StoreRuntimeRetirementResult::Reserved(reservation) => reservation,
        StoreRuntimeRetirementResult::Blocked(refusal) => {
            panic!("completed schema admission retained runtime clients: {refusal:?}")
        }
    };
    let committed =
        tracedecay_runtime_core::native_runtime::spawn_blocking(move || reservation.commit())
            .await
            .unwrap()
            .unwrap();
    assert!(
        matches!(
            committed.outcomes(),
            [StoreRuntimeRetirementOutcome::Closed { .. }]
        ),
        "fixture runtime must close before inspecting or reopening its store: {:?}",
        committed.outcomes()
    );
}

/// A newly owned profile-sessions store. Prospective publication installs
/// the schema before daemon admission; an unmarked existing SQLite file
/// cannot be used as a freshly owned initialization fixture.
struct RegisteredStore {
    path: PathBuf,
    _scope: DaemonDatabaseScope,
    _directory: TempDir,
}

impl RegisteredStore {
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
        let mode = if self.path.try_exists().unwrap() {
            TestDatabaseRuntimeMode::Existing
        } else {
            TestDatabaseRuntimeMode::Initialize
        };
        Database::publish_registered_daemon_test_runtime_with_retirement_control(
            &self.path,
            &authority,
            mode,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap()
    }

    /// Inspect a completed family with an ordinary independent read-only SQLite
    /// connection. Reopening the registered runtime for every observation would
    /// perform another full attachment lifecycle and obscure cancellation timing.
    fn external_schema_objects(&self) -> Vec<(String, Option<String>)> {
        let connection = rusqlite::Connection::open_with_flags(
            &self.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut statement = connection
            .prepare(
                "SELECT type || ':' || name, sql FROM sqlite_schema
                 WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
            )
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Runs daemon admission, cancelling its token just before the
    /// `cancel_at`-th poll, so sweeping `cancel_at` samples cancellation
    /// points across admission. Returns the outcome and how many polls
    /// admission took.
    async fn attach_cancelled_at(&self, cancel_at: usize) -> (Result<(), TraceDecayError>, usize) {
        let (owner, runtime, retirement) = self.publish().await.into_parts();
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
        // The next sample reopens this path. Dropping the admission future
        // releases its owner; exact retirement then joins the physical workers.
        drop(admission);
        drop(runtime);
        retire_runtime(retirement).await;
        (outcome, polls)
    }
}

#[tokio::test]
async fn unpublished_existing_wal_is_refused_without_deleting_foreign_bytes() {
    let store = RegisteredStore::create();
    let raw = rusqlite::Connection::open(&store.path).unwrap();
    raw.execute_batch("CREATE TABLE td_runtime_unpublished_probe(value INTEGER);")
        .unwrap();
    drop(raw);
    let original_main = std::fs::read(&store.path).unwrap();
    let wal = store.path.with_file_name(format!(
        "{}-wal",
        store.path.file_name().unwrap().to_string_lossy()
    ));
    let foreign_wal = b"unclaimed foreign WAL bytes";
    std::fs::write(&wal, foreign_wal).unwrap();

    let authority =
        DatabaseAuthority::for_owned_runtime(&store.path, "unpublished sidecar admission").unwrap();
    let attempted = Database::publish_registered_daemon_test_runtime_with_retirement_control(
        &store.path,
        &authority,
        TestDatabaseRuntimeMode::Existing,
        TestDatabaseRuntimeScope::ProfileSessions,
    )
    .await;
    let error = match attempted {
        Ok(_) => panic!("an unmarked SQLite namespace must not claim a foreign WAL"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("sidecar ownership is ambiguous"),
        "unexpected refusal: {error}"
    );
    assert_eq!(std::fs::read(&store.path).unwrap(), original_main);
    assert_eq!(std::fs::read(&wal).unwrap(), foreign_wal);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_daemon_attach_preserves_published_schema_and_reopens() {
    let store = RegisteredStore::create();
    let (outcome, total_polls) = store.attach_cancelled_at(usize::MAX).await;
    outcome.unwrap();
    let installed = store.external_schema_objects();
    assert!(
        installed
            .iter()
            .any(|(object, _)| object == "table:code_projects"),
        "a completed attach installs the registered schema: {installed:?}"
    );

    let stride = (total_polls / 20).max(1);
    let mut cancelled = Vec::new();
    let mut completed = Vec::new();
    for cancel_at in std::iter::once(1)
        .chain((2..total_polls).step_by(stride))
        .chain([total_polls + 10])
    {
        // The reference admission, then each successful reopen below, leave
        // this family closed with the exact baseline schema. Creating another
        // family would repeat initialization without exercising another
        // cancellation point. Keep real runtime retirement and reopening
        // around every sampled admission.
        let before_admission = store.external_schema_objects();
        assert_eq!(before_admission, installed);
        let (outcome, _) = store.attach_cancelled_at(cancel_at).await;
        let objects = store.external_schema_objects();
        match outcome {
            Err(error) => {
                assert!(
                    error.is_store_open_cancelled(),
                    "cancel at poll {cancel_at}/{total_polls} must fail typed: {error:?}"
                );
                assert_eq!(
                    objects, before_admission,
                    "cancel at poll {cancel_at}/{total_polls} must preserve the published schema"
                );
                cancelled.push(cancel_at);
            }
            Ok(()) => {
                assert_eq!(
                    objects, installed,
                    "an admission completed before cancel at poll {cancel_at}/{total_polls} preserves the published schema"
                );
                completed.push(cancel_at);
            }
        }
        let (reopened, _) = store.attach_cancelled_at(usize::MAX).await;
        reopened.unwrap_or_else(|error| {
            panic!("reopen after cancel at poll {cancel_at}/{total_polls} failed: {error:?}")
        });
        assert_eq!(store.external_schema_objects(), installed);
    }
    assert!(
        cancelled.contains(&1),
        "a cancel before admission starts must stop it; cancelled at {cancelled:?}"
    );
    assert!(completed.contains(&(total_polls + 10)));
}
