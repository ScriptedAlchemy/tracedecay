use std::fmt;

use thiserror::Error;
use tracedecay_application::now_micros;
use tracedecay_domain::{
    PrivacyDomainId, RetrievalCursorKeyId, SessionCursorKeyIdV1, SessionCursorVersionV1,
    SignedCursorKeyRefV1, canonical_sha256,
};
use tracedecay_query::retrieval::QUERY_CURSOR_TTL_MICROS_V1;
use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;
use tracedecay_store::{SessionStoreError, SessionStoreResult};

use tracedecay_runtime_core::db::{
    DatabaseEngineReadSnapshot,
    engine::{Executor, params},
};
use tracedecay_temporal_query::cursor::{CURSOR_CLOCK_SKEW_MICROS, CURSOR_LIFETIME_MICROS};
use tracedecay_temporal_query::ports::{
    CursorKeyError, CursorSignature, InMemoryCursorAuthenticator, SessionCursorAuthenticator,
    TemporalExecutionSnapshot,
};

const LOAD_OPERATION: &str = "load snapshot cursor authentication key";
const PROVISION_OPERATION: &str = "provision active session cursor authentication key";
const CURSOR_KEY_ID_RANDOM_BYTES: usize = 16;
const CURSOR_KEY_MATERIAL_BYTES: usize = 32;
const CURSOR_KEY_RETENTION_MICROS: i64 = CURSOR_LIFETIME_MICROS + CURSOR_CLOCK_SKEW_MICROS;

#[derive(Debug, Error)]
pub enum GlobalDbCursorKeyProviderError {
    #[error("no pre-provisioned active cursor authentication key is available")]
    ActiveKeyMissing,
    #[error("frozen snapshot does not select a cursor authentication key")]
    SnapshotKeyUnavailable,
    #[error("cursor authentication key is unavailable for frozen key {expected:?}")]
    ActiveKeyUnavailable { expected: SignedCursorKeyRefV1 },
    #[error("cursor key authority contains {count} active keys")]
    MultipleActiveKeys { count: i64 },
    #[error("cursor authentication key id is invalid")]
    InvalidKeyId,
    #[error("cursor authentication key version {value} is invalid")]
    InvalidKeyVersion { value: i64 },
    #[error("cursor authentication key material is invalid")]
    InvalidKeyMaterial,
    #[error("cursor authentication key cannot authorize retrieval cursors")]
    InvalidRetrievalKey,
    #[error("failed to provision the active cursor authentication key")]
    Provision {
        #[source]
        source: SessionStoreError,
    },
    #[error("failed to {operation}")]
    Storage {
        operation: &'static str,
        #[source]
        source: tracedecay_runtime_core::db::engine::Error,
    },
}

pub struct GlobalDbCursorKeyProvider {
    active_key: SignedCursorKeyRefV1,
    authenticators: Vec<(SignedCursorKeyRefV1, InMemoryCursorAuthenticator)>,
}

pub(super) async fn ensure_active_session_cursor_key_in_transaction(
    transaction: &impl Executor,
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
    pub async fn from_registered_active(
        read: &DatabaseEngineReadSnapshot,
    ) -> Result<Self, GlobalDbCursorKeyProviderError> {
        let mut rows = read
            .query(
                "SELECT key_id, key_version
                 FROM session_query_cursor_keys
                 WHERE retired_at IS NULL
                 ORDER BY key_version DESC
                 LIMIT 2",
                (),
            )
            .await
            .map_err(storage)?;
        let row = rows
            .next()
            .await
            .map_err(storage)?
            .ok_or(GlobalDbCursorKeyProviderError::ActiveKeyMissing)?;
        if rows.next().await.map_err(storage)?.is_some() {
            return Err(GlobalDbCursorKeyProviderError::MultipleActiveKeys { count: 2 });
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
        drop(rows);
        Self::from_registered_key_ref(read, SignedCursorKeyRefV1 { key_id, version }).await
    }

    pub fn active_key_ref(&self) -> &SignedCursorKeyRefV1 {
        &self.active_key
    }

    pub async fn from_registered_key_ref(
        read: &DatabaseEngineReadSnapshot,
        expected: SignedCursorKeyRefV1,
    ) -> Result<Self, GlobalDbCursorKeyProviderError> {
        Self::from_registered_key_ref_at(read, expected, now_micros().0).await
    }

    pub async fn from_registered_snapshot(
        read: &DatabaseEngineReadSnapshot,
        snapshot: &TemporalExecutionSnapshot,
    ) -> Result<Self, GlobalDbCursorKeyProviderError> {
        let expected = snapshot
            .cursor_key()
            .cloned()
            .ok_or(GlobalDbCursorKeyProviderError::SnapshotKeyUnavailable)?;
        Self::from_registered_key_ref_at(read, expected, now_micros().0).await
    }

    async fn from_registered_key_ref_at(
        read: &DatabaseEngineReadSnapshot,
        expected: SignedCursorKeyRefV1,
        now_micros: i64,
    ) -> Result<Self, GlobalDbCursorKeyProviderError> {
        let retention_cutoff = now_micros.saturating_sub(CURSOR_KEY_RETENTION_MICROS);
        let mut rows = read
            .query(
                "SELECT key_id, key_version, key_material, retired_at,
                        SUM(CASE WHEN retired_at IS NULL THEN 1 ELSE 0 END) OVER ()
                 FROM session_query_cursor_keys
                 WHERE retired_at IS NULL OR retired_at > ?1
                 ORDER BY key_version DESC",
                [retention_cutoff],
            )
            .await
            .map_err(storage)?;
        let mut active_key = None;
        let mut expected_loaded = false;
        let mut authenticators = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage)? {
            let count = row.get::<i64>(4).map_err(storage)?;
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
            let key = SignedCursorKeyRefV1 { key_id, version };
            let retired_at = row.get::<Option<i64>>(3).map_err(storage)?;
            if retired_at.is_none() {
                active_key = Some(key.clone());
            }
            expected_loaded |= key == expected;
            let material = row.get::<Vec<u8>>(2).map_err(storage)?;
            let authenticator = InMemoryCursorAuthenticator::new(key.clone(), material)
                .map_err(|_| GlobalDbCursorKeyProviderError::InvalidKeyMaterial)?;
            authenticators.push((key, authenticator));
        }
        if !expected_loaded {
            return Err(GlobalDbCursorKeyProviderError::ActiveKeyUnavailable { expected });
        }
        let active_key =
            active_key.ok_or(GlobalDbCursorKeyProviderError::ActiveKeyUnavailable { expected })?;
        Ok(Self {
            active_key,
            authenticators,
        })
    }
    pub fn retrieval_keyring(
        &self,
        privacy_domain: PrivacyDomainId,
    ) -> Result<RetrievalCursorKeyringV1, GlobalDbCursorKeyProviderError> {
        let derivation_context =
            canonical_sha256(&("tracedecay.query-cursor-key.v1", &privacy_domain))
                .map_err(|_| GlobalDbCursorKeyProviderError::InvalidRetrievalKey)?;
        let active_authenticator = self
            .authenticators
            .iter()
            .find(|(key, _)| key == &self.active_key)
            .ok_or_else(|| GlobalDbCursorKeyProviderError::ActiveKeyUnavailable {
                expected: self.active_key.clone(),
            })?;
        let active_id = retrieval_key_id(&self.active_key)?;
        let active_epoch = u64::from(self.active_key.version.value());
        let active_material = active_authenticator
            .1
            .derive_key_material(&self.active_key, derivation_context.as_str().as_bytes())
            .map_err(|_| GlobalDbCursorKeyProviderError::InvalidRetrievalKey)?;
        let mut keyring = RetrievalCursorKeyringV1::new(
            privacy_domain,
            active_id,
            active_epoch,
            active_material.to_vec(),
            QUERY_CURSOR_TTL_MICROS_V1,
        )
        .map_err(|_| GlobalDbCursorKeyProviderError::InvalidRetrievalKey)?;
        for (key, authenticator) in &self.authenticators {
            if key == &self.active_key {
                continue;
            }
            let material = authenticator
                .derive_key_material(key, derivation_context.as_str().as_bytes())
                .map_err(|_| GlobalDbCursorKeyProviderError::InvalidRetrievalKey)?;
            keyring
                .retain(
                    retrieval_key_id(key)?,
                    u64::from(key.version.value()),
                    material.to_vec(),
                )
                .map_err(|_| GlobalDbCursorKeyProviderError::InvalidRetrievalKey)?;
        }
        Ok(keyring)
    }
}

fn retrieval_key_id(
    key: &SignedCursorKeyRefV1,
) -> Result<RetrievalCursorKeyId, GlobalDbCursorKeyProviderError> {
    RetrievalCursorKeyId::new(key.key_id.as_str())
        .map_err(|_| GlobalDbCursorKeyProviderError::InvalidRetrievalKey)
}

impl fmt::Debug for GlobalDbCursorKeyProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GlobalDbCursorKeyProvider")
            .field("active_key", &self.active_key)
            .field("verification_key_count", &self.authenticators.len())
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
        if key != &self.active_key {
            return Err(CursorKeyError::Unavailable);
        }
        self.authenticators
            .iter()
            .find(|(candidate, _)| candidate == key)
            .ok_or(CursorKeyError::Unavailable)?
            .1
            .sign(key, authenticated)
    }

    fn verify(
        &self,
        key: &SignedCursorKeyRefV1,
        authenticated: &[u8],
        signature: &CursorSignature,
    ) -> Result<(), CursorKeyError> {
        self.authenticators
            .iter()
            .find(|(candidate, _)| candidate == key)
            .ok_or(CursorKeyError::Unavailable)?
            .1
            .verify(key, authenticated, signature)
    }
}

fn storage(source: tracedecay_runtime_core::db::engine::Error) -> GlobalDbCursorKeyProviderError {
    GlobalDbCursorKeyProviderError::Storage {
        operation: LOAD_OPERATION,
        source,
    }
}
