use std::fmt;

use libsql::{Transaction, params};
use thiserror::Error;
use tracedecay_domain::{SessionCursorKeyIdV1, SessionCursorVersionV1, SignedCursorKeyRefV1};
use tracedecay_store::{SessionStoreError, SessionStoreResult};

use crate::global_db::{GlobalDb, GlobalDbReadSnapshot};
use crate::query::temporal::ports::{
    CursorKeyError, CursorSignature, InMemoryCursorAuthenticator, SessionCursorAuthenticator,
    TemporalExecutionSnapshot,
};

const LOAD_OPERATION: &str = "load snapshot cursor authentication key";
const PROVISION_OPERATION: &str = "provision active session cursor authentication key";
const CURSOR_KEY_ID_RANDOM_BYTES: usize = 16;
const CURSOR_KEY_MATERIAL_BYTES: usize = 32;

#[derive(Debug, Error)]
pub(crate) enum GlobalDbCursorKeyProviderError {
    #[error("frozen snapshot does not select a cursor authentication key")]
    SnapshotKeyUnavailable,
    #[error("active cursor authentication key is unavailable for frozen key {expected:?}")]
    ActiveKeyUnavailable { expected: SignedCursorKeyRefV1 },
    #[error("cursor key authority contains {count} active keys")]
    MultipleActiveKeys { count: i64 },
    #[error("cursor authentication key rotated from {expected:?} to {active:?}")]
    Rotated {
        expected: SignedCursorKeyRefV1,
        active: SignedCursorKeyRefV1,
    },
    #[error("active cursor authentication key id is invalid")]
    InvalidKeyId,
    #[error("active cursor authentication key version {value} is invalid")]
    InvalidKeyVersion { value: i64 },
    #[error("active cursor authentication key material is invalid")]
    InvalidKeyMaterial,
    #[error("failed to {operation}")]
    Storage {
        operation: &'static str,
        #[source]
        source: libsql::Error,
    },
}

pub(crate) struct GlobalDbCursorKeyProvider {
    key: SignedCursorKeyRefV1,
    authenticator: InMemoryCursorAuthenticator,
}

impl GlobalDb {
    pub(crate) async fn ensure_active_session_cursor_key_result(
        &self,
    ) -> SessionStoreResult<SignedCursorKeyRefV1> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?;
        let key = ensure_active_session_cursor_key_in_transaction(&transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?;
        Ok(key)
    }
}

pub(super) async fn ensure_active_session_cursor_key_in_transaction(
    transaction: &Transaction,
) -> SessionStoreResult<SignedCursorKeyRefV1> {
    let mut active_rows = transaction
        .query(
            "SELECT key_id, key_version, key_material, COUNT(*) OVER ()
             FROM session_query_cursor_keys
             WHERE retired_at IS NULL
             ORDER BY key_version DESC
             LIMIT 1",
            (),
        )
        .await
        .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?;
    if let Some(row) = active_rows
        .next()
        .await
        .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?
    {
        let count = row
            .get::<i64>(3)
            .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?;
        if count != 1 {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "active session cursor key count",
            });
        }
        let key_id = SessionCursorKeyIdV1::new(
            row.get::<String>(0)
                .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?,
        )
        .map_err(SessionStoreError::from)?;
        let version_value = row
            .get::<i64>(1)
            .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?;
        let version = u16::try_from(version_value)
            .ok()
            .and_then(|value| SessionCursorVersionV1::new(value).ok())
            .ok_or(SessionStoreError::InvalidStateTransition {
                context: "active session cursor key version",
            })?;
        let key = SignedCursorKeyRefV1 { key_id, version };
        let material = row
            .get::<Vec<u8>>(2)
            .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?;
        InMemoryCursorAuthenticator::new(key.clone(), material).map_err(|_| {
            SessionStoreError::InvalidStateTransition {
                context: "active session cursor key material",
            }
        })?;
        drop(active_rows);
        return Ok(key);
    }
    drop(active_rows);

    let mut history_rows = transaction
        .query(
            "SELECT COALESCE(MAX(key_version), 0), COALESCE(MAX(created_at), 0)
             FROM session_query_cursor_keys",
            (),
        )
        .await
        .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?;
    let history = history_rows
        .next()
        .await
        .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?
        .ok_or(SessionStoreError::InvalidStateTransition {
            context: "session cursor key history",
        })?;
    let highest_version = history
        .get::<i64>(0)
        .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?;
    let highest_created_at = history
        .get::<i64>(1)
        .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?;
    drop(history_rows);

    let next_version = highest_version
        .checked_add(1)
        .and_then(|value| u16::try_from(value).ok())
        .and_then(|value| SessionCursorVersionV1::new(value).ok())
        .ok_or(SessionStoreError::InvalidStateTransition {
            context: "session cursor key version exhausted",
        })?;
    let mut key_id_random = [0_u8; CURSOR_KEY_ID_RANDOM_BYTES];
    getrandom::getrandom(&mut key_id_random).map_err(|error| {
        super::query::storage(
            PROVISION_OPERATION,
            std::io::Error::other(format!("generate session cursor key id: {error}")),
        )
    })?;
    let key_id = SessionCursorKeyIdV1::new(format!(
        "cursor-key-{}-{}",
        next_version.value(),
        hex::encode(key_id_random)
    ))
    .map_err(SessionStoreError::from)?;
    let key = SignedCursorKeyRefV1 {
        key_id,
        version: next_version,
    };
    let mut material = [0_u8; CURSOR_KEY_MATERIAL_BYTES];
    getrandom::getrandom(&mut material).map_err(|error| {
        super::query::storage(
            PROVISION_OPERATION,
            std::io::Error::other(format!("generate session cursor key material: {error}")),
        )
    })?;
    let minimum_created_at =
        highest_created_at
            .checked_add(1)
            .ok_or(SessionStoreError::InvalidStateTransition {
                context: "session cursor key timestamp exhausted",
            })?;
    let created_at = super::query::now_micros(PROVISION_OPERATION)?
        .0
        .max(minimum_created_at);
    transaction
        .execute(
            "INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES (?1, ?2, ?3, ?4, NULL)",
            params![
                key.key_id.as_str(),
                i64::from(key.version.value()),
                material.to_vec(),
                created_at,
            ],
        )
        .await
        .map_err(|error| super::query::storage(PROVISION_OPERATION, error))?;
    Ok(key)
}

impl GlobalDbCursorKeyProvider {
    pub(crate) async fn from_snapshot(
        read: &GlobalDbReadSnapshot,
        snapshot: &TemporalExecutionSnapshot,
    ) -> Result<Self, GlobalDbCursorKeyProviderError> {
        let expected = snapshot
            .cursor_key()
            .cloned()
            .ok_or(GlobalDbCursorKeyProviderError::SnapshotKeyUnavailable)?;
        Self::from_key_ref(read, expected).await
    }

    pub(crate) async fn from_key_ref(
        read: &GlobalDbReadSnapshot,
        expected: SignedCursorKeyRefV1,
    ) -> Result<Self, GlobalDbCursorKeyProviderError> {
        let mut rows = read
            .query(
                "SELECT key_id, key_version, key_material, COUNT(*) OVER ()
                 FROM session_query_cursor_keys
                 WHERE retired_at IS NULL
                 ORDER BY key_version DESC
                 LIMIT 1",
                (),
            )
            .await
            .map_err(storage)?;
        let Some(row) = rows.next().await.map_err(storage)? else {
            return Err(GlobalDbCursorKeyProviderError::ActiveKeyUnavailable { expected });
        };
        let count = row.get::<i64>(3).map_err(storage)?;
        if count != 1 {
            return Err(GlobalDbCursorKeyProviderError::MultipleActiveKeys { count });
        }

        let key_id = SessionCursorKeyIdV1::new(row.get::<String>(0).map_err(storage)?)
            .map_err(|_| GlobalDbCursorKeyProviderError::InvalidKeyId)?;
        let version_value = row.get::<i64>(1).map_err(storage)?;
        let version = u16::try_from(version_value)
            .ok()
            .and_then(|value| SessionCursorVersionV1::new(value).ok())
            .ok_or(GlobalDbCursorKeyProviderError::InvalidKeyVersion {
                value: version_value,
            })?;
        let active = SignedCursorKeyRefV1 { key_id, version };
        if active != expected {
            return Err(GlobalDbCursorKeyProviderError::Rotated { expected, active });
        }

        let material = row.get::<Vec<u8>>(2).map_err(storage)?;
        let authenticator = InMemoryCursorAuthenticator::new(active.clone(), material)
            .map_err(|_| GlobalDbCursorKeyProviderError::InvalidKeyMaterial)?;
        Ok(Self {
            key: active,
            authenticator,
        })
    }
}

impl fmt::Debug for GlobalDbCursorKeyProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GlobalDbCursorKeyProvider")
            .field("key", &self.key)
            .field("secret", &"REDACTED")
            .finish()
    }
}

impl SessionCursorAuthenticator for GlobalDbCursorKeyProvider {
    fn sign(
        &self,
        key: &SignedCursorKeyRefV1,
        authenticated: &[u8],
    ) -> Result<CursorSignature, CursorKeyError> {
        self.authenticator.sign(key, authenticated)
    }

    fn verify(
        &self,
        key: &SignedCursorKeyRefV1,
        authenticated: &[u8],
        signature: &CursorSignature,
    ) -> Result<(), CursorKeyError> {
        self.authenticator.verify(key, authenticated, signature)
    }
}

fn storage(source: libsql::Error) -> GlobalDbCursorKeyProviderError {
    GlobalDbCursorKeyProviderError::Storage {
        operation: LOAD_OPERATION,
        source,
    }
}

#[cfg(test)]
mod tests {
    use libsql::params;
    use tempfile::TempDir;
    use tracedecay_domain::{
        RetrievalGrainV1, SessionCursorKeyIdV1, SessionCursorVersionV1, SessionId,
        SignedCursorKeyRefV1, TemporalModeV1,
    };

    use super::*;
    use crate::global_db::GlobalDb;
    use crate::query::temporal::ports::{
        BindingDigest, CursorKeyError, KernelVersions, SessionCursorAuthenticator,
        TemporalExecutionSnapshot, TemporalSnapshotRequest, TemporalWatermarks,
    };

    const KEY_ONE_MATERIAL: [u8; 32] = [0xa5; 32];
    const KEY_TWO_MATERIAL: [u8; 32] = [0x5a; 32];

    fn key_ref(key_id: &str, version: u16) -> SignedCursorKeyRefV1 {
        SignedCursorKeyRefV1 {
            key_id: SessionCursorKeyIdV1::new(key_id).expect("valid key id"),
            version: SessionCursorVersionV1::new(version).expect("valid key version"),
        }
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn snapshot(cursor_key: Option<SignedCursorKeyRefV1>) -> TemporalExecutionSnapshot {
        TemporalExecutionSnapshot::new(
            TemporalSnapshotRequest::new(
                SessionId::new("session-1").expect("valid session id"),
                digest('1'),
                digest('2'),
                digest('3'),
                TemporalModeV1::Current,
                RetrievalGrainV1::Session,
            )
            .expect("valid snapshot request"),
            TemporalWatermarks {
                generation: 1,
                source: 2,
                projection: 3,
                index: 4,
                summary: 5,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration", digest('4'))
                    .expect("valid configuration digest"),
            },
            cursor_key,
        )
        .expect("authorized test snapshot")
    }

    async fn open_db(temp: &TempDir) -> GlobalDb {
        GlobalDb::try_open_at(&temp.path().join("global.db"))
            .await
            .expect("database open should succeed")
            .expect("database should be available")
    }

    async fn insert_key(
        db: &GlobalDb,
        key_id: &str,
        key_version: i64,
        material: &[u8],
        created_at: i64,
    ) {
        db.read_connection()
            .execute(
                "INSERT INTO session_query_cursor_keys (
                    key_id, key_version, key_material, created_at, retired_at
                 ) VALUES (?1, ?2, ?3, ?4, NULL)",
                params![key_id, key_version, material.to_vec(), created_at],
            )
            .await
            .expect("cursor key insert should succeed");
    }

    async fn key_rows(db: &GlobalDb) -> Vec<(String, i64, Vec<u8>, i64, Option<i64>)> {
        let mut rows = db
            .read_connection()
            .query(
                "SELECT key_id, key_version, key_material, created_at, retired_at
                 FROM session_query_cursor_keys
                 ORDER BY key_version",
                (),
            )
            .await
            .expect("cursor key query should succeed");
        let mut values = Vec::new();
        while let Some(row) = rows.next().await.expect("cursor key row should decode") {
            values.push((
                row.get(0).expect("key id"),
                row.get(1).expect("key version"),
                row.get(2).expect("key material"),
                row.get(3).expect("created at"),
                row.get(4).expect("retired at"),
            ));
        }
        values
    }

    async fn load_provider(
        db: &GlobalDb,
        snapshot: &TemporalExecutionSnapshot,
    ) -> Result<GlobalDbCursorKeyProvider, GlobalDbCursorKeyProviderError> {
        let read = db.read_snapshot().await.expect("read snapshot");
        GlobalDbCursorKeyProvider::from_snapshot(&read, snapshot).await
    }

    #[tokio::test]
    async fn reconstructs_the_snapshot_selected_key_after_restart() {
        let temp = TempDir::new().expect("temporary directory");
        let db_path = temp.path().join("global.db");
        let db = open_db(&temp).await;
        insert_key(&db, "cursor-key-1", 1, &KEY_ONE_MATERIAL, 100).await;
        let frozen = snapshot(Some(key_ref("cursor-key-1", 1)));
        let before_restart = load_provider(&db, &frozen)
            .await
            .expect("provider should load");
        let signature = before_restart
            .sign(
                frozen.cursor_key().expect("snapshot key"),
                b"restart-stable",
            )
            .expect("signature should be created");
        drop(before_restart);
        drop(db);

        let reopened = GlobalDb::try_open_at(&db_path)
            .await
            .expect("database reopen should succeed")
            .expect("database should be available");
        let reconstructed = load_provider(&reopened, &frozen)
            .await
            .expect("provider should reconstruct");
        reconstructed
            .verify(
                frozen.cursor_key().expect("snapshot key"),
                b"restart-stable",
                &signature,
            )
            .expect("reconstructed provider should verify the signature");
    }

    #[tokio::test]
    async fn loaded_provider_never_requeries_after_rotation() {
        let temp = TempDir::new().expect("temporary directory");
        let db = open_db(&temp).await;
        insert_key(&db, "cursor-key-1", 1, &KEY_ONE_MATERIAL, 100).await;
        let original_key = key_ref("cursor-key-1", 1);
        let original_snapshot = snapshot(Some(original_key.clone()));
        let read = db.read_snapshot().await.expect("read snapshot");
        let mut freeze_rows = read
            .query("SELECT COUNT(*) FROM session_query_cursor_keys", ())
            .await
            .expect("freeze snapshot");
        assert_eq!(
            freeze_rows
                .next()
                .await
                .expect("freeze row")
                .expect("count row")
                .get::<i64>(0)
                .expect("count"),
            1
        );
        drop(freeze_rows);

        let provider = GlobalDbCursorKeyProvider::from_snapshot(&read, &original_snapshot)
            .await
            .expect("provider should load");

        insert_key(&db, "cursor-key-2", 2, &KEY_TWO_MATERIAL, 200).await;

        let signature = provider
            .sign(&original_key, b"frozen")
            .expect("loaded provider should retain its frozen key");
        provider
            .verify(&original_key, b"frozen", &signature)
            .expect("loaded provider should not consult the rotated database");
        assert!(matches!(
            provider.sign(&key_ref("cursor-key-2", 2), b"rotated"),
            Err(CursorKeyError::Unavailable)
        ));

        let rotated_snapshot = snapshot(Some(key_ref("cursor-key-2", 2)));
        let fresh_read = db.read_snapshot().await.expect("fresh read snapshot");
        GlobalDbCursorKeyProvider::from_snapshot(&fresh_read, &rotated_snapshot)
            .await
            .expect("new snapshot should observe rotation");
    }

    #[tokio::test]
    async fn reports_rotation_against_the_frozen_snapshot() {
        let temp = TempDir::new().expect("temporary directory");
        let db = open_db(&temp).await;
        insert_key(&db, "cursor-key-1", 1, &KEY_ONE_MATERIAL, 100).await;
        let frozen = snapshot(Some(key_ref("cursor-key-1", 1)));
        insert_key(&db, "cursor-key-2", 2, &KEY_TWO_MATERIAL, 200).await;

        let error = load_provider(&db, &frozen)
            .await
            .expect_err("stale snapshot must report rotation");
        assert!(matches!(
            error,
            GlobalDbCursorKeyProviderError::Rotated {
                expected,
                active,
            } if expected == key_ref("cursor-key-1", 1)
                && active == key_ref("cursor-key-2", 2)
        ));
    }

    #[tokio::test]
    async fn missing_keys_are_unavailable_and_reads_create_nothing() {
        let temp = TempDir::new().expect("temporary directory");
        let db = open_db(&temp).await;
        let frozen = snapshot(Some(key_ref("cursor-key-1", 1)));

        let error = load_provider(&db, &frozen)
            .await
            .expect_err("missing active key must fail closed");
        assert!(matches!(
            error,
            GlobalDbCursorKeyProviderError::ActiveKeyUnavailable { expected }
                if expected == key_ref("cursor-key-1", 1)
        ));
        assert!(key_rows(&db).await.is_empty());

        let no_key_snapshot = snapshot(None);
        assert!(matches!(
            load_provider(&db, &no_key_snapshot).await,
            Err(GlobalDbCursorKeyProviderError::SnapshotKeyUnavailable)
        ));
        assert!(key_rows(&db).await.is_empty());
    }

    #[tokio::test]
    async fn multiple_active_keys_are_rejected() {
        let temp = TempDir::new().expect("temporary directory");
        let db = open_db(&temp).await;
        db.read_connection()
            .execute_batch(
                "DROP TRIGGER session_query_cursor_keys_insert_guard_v1;
                 DROP TRIGGER session_query_cursor_keys_rotate_insert_v1;
                 DROP TRIGGER session_query_cursor_keys_retire_update_v1;",
            )
            .await
            .expect("test should disable key guards");
        insert_key(&db, "cursor-key-1", 1, &KEY_ONE_MATERIAL, 100).await;
        insert_key(&db, "cursor-key-2", 2, &KEY_TWO_MATERIAL, 200).await;

        assert!(matches!(
            load_provider(&db, &snapshot(Some(key_ref("cursor-key-2", 2))),).await,
            Err(GlobalDbCursorKeyProviderError::MultipleActiveKeys { count: 2 })
        ));
    }

    #[tokio::test]
    async fn corrupt_key_identity_is_rejected_without_echoing_it() {
        let temp = TempDir::new().expect("temporary directory");
        let db = open_db(&temp).await;
        insert_key(&db, "corrupt\nkey", 1, &KEY_ONE_MATERIAL, 100).await;

        let error = load_provider(&db, &snapshot(Some(key_ref("cursor-key-1", 1))))
            .await
            .expect_err("corrupt key identity must fail closed");
        assert!(matches!(
            error,
            GlobalDbCursorKeyProviderError::InvalidKeyId
        ));
        assert!(!error.to_string().contains("corrupt"));
    }

    #[tokio::test]
    async fn corrupt_key_version_is_rejected() {
        let temp = TempDir::new().expect("temporary directory");
        let db = open_db(&temp).await;
        insert_key(&db, "cursor-key-overflow", 65_536, &KEY_ONE_MATERIAL, 100).await;

        assert!(matches!(
            load_provider(&db, &snapshot(Some(key_ref("cursor-key-1", 1))),).await,
            Err(GlobalDbCursorKeyProviderError::InvalidKeyVersion { value: 65_536 })
        ));
    }

    #[tokio::test]
    async fn short_material_is_rejected_and_debug_is_redacted() {
        let temp = TempDir::new().expect("temporary directory");
        let db = open_db(&temp).await;
        insert_key(&db, "cursor-key-1", 1, &[0xa5; 31], 100).await;
        let frozen = snapshot(Some(key_ref("cursor-key-1", 1)));
        assert!(matches!(
            load_provider(&db, &frozen).await,
            Err(GlobalDbCursorKeyProviderError::InvalidKeyMaterial)
        ));

        db.read_connection()
            .execute(
                "INSERT INTO session_query_cursor_keys (
                    key_id, key_version, key_material, created_at, retired_at
                 ) VALUES ('cursor-key-2', 2, ?1, 200, NULL)",
                params![KEY_TWO_MATERIAL.to_vec()],
            )
            .await
            .expect("rotation should succeed");
        let provider = load_provider(&db, &snapshot(Some(key_ref("cursor-key-2", 2))))
            .await
            .expect("valid provider should load");
        let debug = format!("{provider:?}");
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains("[90, 90"));
        assert!(!debug.contains("key_material"));
    }

    #[tokio::test]
    async fn tampering_fails_authentication() {
        let temp = TempDir::new().expect("temporary directory");
        let db = open_db(&temp).await;
        insert_key(&db, "cursor-key-1", 1, &KEY_ONE_MATERIAL, 100).await;
        let key = key_ref("cursor-key-1", 1);
        let provider = load_provider(&db, &snapshot(Some(key.clone())))
            .await
            .expect("provider should load");
        let signature = provider
            .sign(&key, b"authenticated")
            .expect("signature should be created");

        assert!(matches!(
            provider.verify(&key, b"tampered", &signature),
            Err(CursorKeyError::AuthenticationFailed)
        ));
    }

    #[tokio::test]
    async fn loading_and_authentication_are_read_only() {
        let temp = TempDir::new().expect("temporary directory");
        let db = open_db(&temp).await;
        insert_key(&db, "cursor-key-1", 1, &KEY_ONE_MATERIAL, 100).await;
        let before = key_rows(&db).await;
        let key = key_ref("cursor-key-1", 1);
        let provider = load_provider(&db, &snapshot(Some(key.clone())))
            .await
            .expect("provider should load");
        let signature = provider
            .sign(&key, b"read-only")
            .expect("signature should be created");
        provider
            .verify(&key, b"read-only", &signature)
            .expect("signature should verify");

        assert_eq!(key_rows(&db).await, before);
    }
}
