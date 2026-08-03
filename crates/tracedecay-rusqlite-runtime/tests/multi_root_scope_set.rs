use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use rusqlite::{Connection, Savepoint};
use tempfile::TempDir;
use tracedecay_application::{
    AuthorizedScopeSet, AuthorizedScopeSetAuthority, CancellationContext, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, LocatorDigest, ManifestDigest, ProjectId, RefId, RepositoryId, ScopeSetId,
    ScopeSetRevision, UtcMicros, WorktreeId,
};
use tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle;
use tracedecay_rusqlite_runtime::reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor};
use tracedecay_rusqlite_runtime::repository::{
    AUTHORIZED_SCOPE_SET_SCHEMA_V1, AuthorizedScopeSetExecutor, AuthorizedScopeSetSqliteStorage,
};
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
};
use tracedecay_store::runtime::ScopeSetCasOutcomeV1;
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    StorageRuntimeErrorV1, StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

const CAPABILITY: &str = "capability.multi-root.query";
const USE_CASE: &str = "use-case.multi-root.query";

struct NoTypedWrites;

impl StorageOperationExecutor for NoTypedWrites {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        unreachable!("scope sets use only the registered exact SQL channel")
    }
}

#[derive(Clone)]
struct NoTypedReads;

impl ReaderQueryExecutor for NoTypedReads {
    fn execute_read(
        &mut self,
        _snapshot: &rusqlite::Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        unreachable!("scope sets use only the registered exact SQL channel")
    }
}

struct RegisteredScopeSetStore {
    storage: AuthorizedScopeSetSqliteStorage,
    path: PathBuf,
    _writer: PersistentWriter,
    _readers: ReaderPool<NoTypedReads>,
    _directory: TempDir,
}

impl RegisteredScopeSetStore {
    fn start(name: &str, setup: impl FnOnce(&Connection)) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("{name}.sqlite3"));
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(AUTHORIZED_SCOPE_SET_SCHEMA_V1)
                .unwrap();
            setup(&connection);
        }
        let path = path.canonicalize().unwrap();
        let binding = registered_binding(name);
        let locator = registered_locator(&binding);
        let writer = PersistentWriter::start(
            ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone()).unwrap(),
            AdmissionConfigV1::default(),
            NoTypedWrites,
        )
        .unwrap();
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding, locator, path.clone()).unwrap(),
            AdmissionConfigV1::default().readers,
            NoTypedReads,
        )
        .unwrap();
        let handle = ExactSqlHandle::attach(&writer, &readers).unwrap();
        Self {
            storage: AuthorizedScopeSetSqliteStorage::from_registered(handle),
            path,
            _writer: writer,
            _readers: readers,
            _directory: directory,
        }
    }

    fn inspect(&self, read: impl FnOnce(&Connection)) {
        let connection = Connection::open(&self.path).unwrap();
        read(&connection);
    }
}

fn registered_binding(name: &str) -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.scope-set",
            "profile_id": "profile.scope-set",
            "scope": { "kind": "project", "project_id": format!("project.scope-set.{name}") }
        },
        "incarnation": 1,
        "authority_epoch": 1
    }))
    .unwrap()
}

fn registered_locator(binding: &StoreRuntimeBindingV1) -> VerifiedStoreLocatorV1 {
    VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).unwrap(),
        LocatorDigest::new(format!("sha256:{}", "5".repeat(64))).unwrap(),
    )
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn context_for_actor(worktree: &str, suffix: &str, actor: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.fixture"),
        id::<RepositoryId>("repository.fixture"),
        id::<WorktreeId>(worktree),
        Some(id::<RefId>("refs/heads/main")),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id(&format!("grant.{suffix}")),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([CapabilityId::new(CAPABILITY).unwrap()]),
        BTreeSet::from([UseCaseId::new(USE_CASE).unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>(actor),
        scope,
        grant,
        RequestId::new(format!("request.{suffix}")).unwrap(),
        Deadline::new(UtcMicros(900)).unwrap(),
        CancellationContext::active(format!("cancel.{suffix}")).unwrap(),
    )
    .unwrap()
}

fn scope_set(revision: u64) -> AuthorizedScopeSet {
    scope_set_for_actor(revision, "actor.requester")
}

fn scope_set_for_actor(revision: u64, actor: &str) -> AuthorizedScopeSet {
    scope_set_for_id_actor(revision, "scope-set.fixture", actor)
}

fn scope_set_for_id_actor(revision: u64, scope_set_id: &str, actor: &str) -> AuthorizedScopeSet {
    AuthorizedScopeSetAuthority::authorize(
        ScopeSetId::new(scope_set_id).unwrap(),
        ScopeSetRevision::new(revision).unwrap(),
        vec![
            context_for_actor("worktree.main", &format!("main.{revision}"), actor),
            context_for_actor("worktree.linked", &format!("linked.{revision}"), actor),
        ],
        &CapabilityId::new(CAPABILITY).unwrap(),
        &UseCaseId::new(USE_CASE).unwrap(),
        UtcMicros(10),
    )
    .unwrap()
}

#[test]
fn scope_set_cas_rejects_stale_revision_and_survives_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("scope-sets.db");
    let first = scope_set(1);
    let second = scope_set(2);

    {
        let mut connection = Connection::open(&path).unwrap();
        AuthorizedScopeSetExecutor::install_schema(&connection).unwrap();
        assert!(matches!(
            AuthorizedScopeSetExecutor::compare_and_swap(&mut connection, None, &first).unwrap(),
            ScopeSetCasOutcomeV1::Applied(_)
        ));
        assert!(matches!(
            AuthorizedScopeSetExecutor::compare_and_swap(
                &mut connection,
                Some(ScopeSetRevision::new(1).unwrap()),
                &second,
            )
            .unwrap(),
            ScopeSetCasOutcomeV1::Applied(_)
        ));
        assert!(matches!(
            AuthorizedScopeSetExecutor::compare_and_swap(
                &mut connection,
                Some(ScopeSetRevision::new(1).unwrap()),
                &second,
            )
            .unwrap(),
            ScopeSetCasOutcomeV1::Conflict {
                actual_revision: Some(actual),
                ..
            } if actual == ScopeSetRevision::new(2).unwrap()
        ));
    }

    let reopened = Connection::open(&path).unwrap();
    let restored = AuthorizedScopeSetExecutor::read(&reopened, second.scope_set_id())
        .unwrap()
        .unwrap();
    assert_eq!(restored, second);
}

#[test]
fn scope_set_cas_rejects_cross_actor_update_without_changing_stored_bytes() {
    let mut connection = Connection::open_in_memory().unwrap();
    AuthorizedScopeSetExecutor::install_schema(&connection).unwrap();
    let first = scope_set_for_actor(1, "actor.owner");
    let takeover = scope_set_for_actor(2, "actor.other");
    AuthorizedScopeSetExecutor::compare_and_swap(&mut connection, None, &first).unwrap();
    let before: Vec<u8> = connection
        .query_row(
            "SELECT canonical_payload FROM authorized_scope_sets_v1 WHERE scope_set_id = ?1",
            [first.scope_set_id().as_str()],
            |row| row.get(0),
        )
        .unwrap();

    assert!(
        AuthorizedScopeSetExecutor::compare_and_swap(
            &mut connection,
            Some(ScopeSetRevision::new(1).unwrap()),
            &takeover,
        )
        .is_err()
    );
    let after: Vec<u8> = connection
        .query_row(
            "SELECT canonical_payload FROM authorized_scope_sets_v1 WHERE scope_set_id = ?1",
            [first.scope_set_id().as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(
        AuthorizedScopeSetExecutor::read(&connection, first.scope_set_id())
            .unwrap()
            .unwrap(),
        first
    );
}

#[test]
fn public_scope_set_store_rejects_invalid_revision_and_payload_edges() {
    let canonical = scope_set(1);
    let payload = serde_json::to_vec(&canonical).unwrap();

    for revision in [0_i64, -1_i64] {
        let connection = Connection::open_in_memory().unwrap();
        AuthorizedScopeSetExecutor::install_schema(&connection).unwrap();
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        connection
            .execute(
                "INSERT INTO authorized_scope_sets_v1
                     (scope_set_id, revision, digest, canonical_payload)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    canonical.scope_set_id().as_str(),
                    revision,
                    canonical.digest().as_str(),
                    payload,
                ],
            )
            .unwrap();

        assert!(
            AuthorizedScopeSetExecutor::read(&connection, canonical.scope_set_id()).is_err(),
            "revision {revision} must fail through the public store read"
        );
    }

    let mut overflow_connection = Connection::open_in_memory().unwrap();
    AuthorizedScopeSetExecutor::install_schema(&overflow_connection).unwrap();
    let overflow = scope_set(u64::try_from(i64::MAX).unwrap() + 1);
    assert!(
        AuthorizedScopeSetExecutor::compare_and_swap(&mut overflow_connection, None, &overflow,)
            .is_err()
    );
    let count: i64 = overflow_connection
        .query_row("SELECT COUNT(*) FROM authorized_scope_sets_v1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);

    let corrupt_connection = Connection::open_in_memory().unwrap();
    AuthorizedScopeSetExecutor::install_schema(&corrupt_connection).unwrap();
    corrupt_connection
        .execute(
            "INSERT INTO authorized_scope_sets_v1
                 (scope_set_id, revision, digest, canonical_payload)
             VALUES (?1, 1, ?2, ?3)",
            rusqlite::params![
                canonical.scope_set_id().as_str(),
                canonical.digest().as_str(),
                b"{".as_slice(),
            ],
        )
        .unwrap();
    assert!(
        AuthorizedScopeSetExecutor::read(&corrupt_connection, canonical.scope_set_id()).is_err()
    );
}

#[test]
fn registered_scope_set_store_preserves_actor_and_checked_revisions() {
    let store = RegisteredScopeSetStore::start("actor-cas", |_| {});
    let first = scope_set_for_actor(1, "actor.owner");
    let second = scope_set_for_actor(2, "actor.owner");
    let takeover = scope_set_for_actor(3, "actor.other");

    assert!(matches!(
        store.storage.compare_and_swap(None, &first).unwrap(),
        ScopeSetCasOutcomeV1::Applied(_)
    ));
    assert!(matches!(
        store
            .storage
            .compare_and_swap(Some(ScopeSetRevision::new(1).unwrap()), &second)
            .unwrap(),
        ScopeSetCasOutcomeV1::Applied(_)
    ));
    assert!(
        store
            .storage
            .compare_and_swap(Some(ScopeSetRevision::new(2).unwrap()), &takeover)
            .is_err()
    );
    assert_eq!(
        store.storage.read(first.scope_set_id()).unwrap(),
        Some(second)
    );

    let oversized = scope_set_for_id_actor(
        u64::try_from(i64::MAX).unwrap() + 1,
        "scope-set.overflow",
        "actor.owner",
    );
    assert!(store.storage.compare_and_swap(None, &oversized).is_err());
    store.inspect(|connection| {
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM authorized_scope_sets_v1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    });
}

#[test]
fn registered_scope_set_store_rejects_zero_negative_and_corrupt_rows() {
    let canonical = scope_set(1);
    let payload = serde_json::to_vec(&canonical).unwrap();
    let id = canonical.scope_set_id().as_str().to_owned();
    let digest = canonical.digest().as_str().to_owned();

    for (name, revision) in [("zero-revision", 0_i64), ("negative-revision", -1_i64)] {
        let payload = payload.clone();
        let id = id.clone();
        let digest = digest.clone();
        let store = RegisteredScopeSetStore::start(name, move |connection| {
            connection
                .execute_batch("PRAGMA ignore_check_constraints = ON")
                .unwrap();
            connection
                .execute(
                    "INSERT INTO authorized_scope_sets_v1
                         (scope_set_id, revision, digest, canonical_payload)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![id, revision, digest, payload],
                )
                .unwrap();
        });
        assert!(store.storage.read(canonical.scope_set_id()).is_err());
    }

    let corrupt = RegisteredScopeSetStore::start("corrupt-payload", move |connection| {
        connection
            .execute(
                "INSERT INTO authorized_scope_sets_v1
                     (scope_set_id, revision, digest, canonical_payload)
                 VALUES (?1, 1, ?2, ?3)",
                rusqlite::params![id, digest, b"{".as_slice()],
            )
            .unwrap();
    });
    assert!(corrupt.storage.read(canonical.scope_set_id()).is_err());
}
