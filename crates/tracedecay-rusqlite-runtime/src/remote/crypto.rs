use std::hint::black_box;
use std::sync::Arc;

use ring::aead::{AES_256_GCM, LessSafeKey, UnboundKey};

use super::RemoteSqliteStorageErrorV1;

/// Opaque AEAD key resolved by the daemon's secret authority.
pub struct RemoteSpoolKeyV1 {
    pub(super) revision: u64,
    pub(super) key: LessSafeKey,
}

impl RemoteSpoolKeyV1 {
    pub fn from_secret_bytes(
        revision: u64,
        mut bytes: Vec<u8>,
    ) -> Result<Self, RemoteSqliteStorageErrorV1> {
        if revision == 0 {
            bytes.fill(0);
            black_box(&bytes);
            return Err(RemoteSqliteStorageErrorV1::InvalidKeyRevision);
        }
        if bytes.len() != AES_256_GCM.key_len() {
            bytes.fill(0);
            black_box(&bytes);
            return Err(RemoteSqliteStorageErrorV1::InvalidKeyLength);
        }
        let key = UnboundKey::new(&AES_256_GCM, &bytes)
            .map(LessSafeKey::new)
            .map_err(|_| RemoteSqliteStorageErrorV1::InvalidKeyLength);
        bytes.fill(0);
        black_box(&bytes);
        Ok(Self {
            revision,
            key: key?,
        })
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

pub trait RemoteSpoolKeyringV1: Send + Sync {
    fn active_key(&self) -> Result<Arc<RemoteSpoolKeyV1>, RemoteSqliteStorageErrorV1>;
    fn key(
        &self,
        revision: u64,
    ) -> Result<Option<Arc<RemoteSpoolKeyV1>>, RemoteSqliteStorageErrorV1>;
}
