//! Cross-process Hook V2 configuration publication contracts.
//!
//! A daemon-owned authority atomically publishes these compact bindings as
//! private JSON. Hook processes receive a read-only file adapter and never
//! discover a project from a path or open a TraceDecay store.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::UtcMicros;
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, atomic_write, read_bounded};

use crate::{HookHostV1, HookScopeBindingV1};

pub const HOOK_CONFIGURATION_SCHEMA_VERSION: u16 = 1;
pub const MAX_HOOK_CONFIGURATION_BYTES: usize = 64 * 1024;
const DIRECTORY_SYNC_POLICY: DirectorySyncPolicy = DirectorySyncPolicy::TolerateUnsupported;

pub fn hook_configuration_path(data_root: &Path, host: HookHostV1) -> PathBuf {
    data_root.join(format!("hook-config-{}.json", host.hook_key()))
}

/// Daemon-issued configuration that a hook process can consume. All identity
/// fields reside in the opaque binding; this value has no path, credential,
/// endpoint, prompt, tool payload, or host-local storage selector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookConfigurationSnapshotV1 {
    pub schema_version: u16,
    pub revision: u64,
    pub published_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub binding: HookScopeBindingV1,
}

impl HookConfigurationSnapshotV1 {
    pub fn validate(&self) -> Result<(), HookConfigurationPublicationError> {
        if self.schema_version != HOOK_CONFIGURATION_SCHEMA_VERSION
            || self.revision == 0
            || self.published_at.0 <= 0
            || self.expires_at.0 <= self.published_at.0
        {
            return Err(HookConfigurationPublicationError::InvalidSnapshot);
        }
        self.binding
            .validate()
            .map_err(|_| HookConfigurationPublicationError::InvalidSnapshot)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookConfigurationPublicationOutcomeV1 {
    Published,
    Duplicate,
    StaleRejected,
}

/// Result exposed to a hook process. The states are intentionally content-free
/// and do not disclose a different host's binding or configuration existence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookConfigurationReadOutcomeV1 {
    Bound(HookConfigurationSnapshotV1),
    Missing,
    Stale,
    Corrupted,
    Unavailable,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HookConfigurationPublicationError {
    #[error("hook configuration snapshot is structurally invalid")]
    InvalidSnapshot,
    #[error("hook configuration JSON is malformed or exceeds its bound")]
    Corrupted,
    #[error("hook configuration publication authority is unavailable")]
    Unavailable,
}

/// Daemon-only atomic publication seam.
pub trait HookConfigurationPublicationStoreV1 {
    fn publish(
        &self,
        snapshot: HookConfigurationSnapshotV1,
    ) -> Result<HookConfigurationPublicationOutcomeV1, HookConfigurationPublicationError>;
}

/// Hook-process read-only configuration seam.
pub trait HookConfigurationReadStoreV1 {
    fn load(
        &self,
        host: HookHostV1,
    ) -> Result<Option<HookConfigurationSnapshotV1>, HookConfigurationPublicationError>;
}

/// Daemon-side publisher. Structure and monotonic revision are checked before
/// the atomic file store sees a record.
pub struct HookConfigurationPublisherV1<S> {
    store: S,
}

impl<S> HookConfigurationPublisherV1<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S> HookConfigurationPublisherV1<S>
where
    S: HookConfigurationPublicationStoreV1,
{
    pub fn publish(
        &self,
        snapshot: HookConfigurationSnapshotV1,
    ) -> Result<HookConfigurationPublicationOutcomeV1, HookConfigurationPublicationError> {
        snapshot.validate()?;
        self.store.publish(snapshot)
    }
}

/// Subscriber adapter for a separate hook process. It revalidates schema,
/// revision, exact host/scope binding, and expiry on every bounded read.
pub struct HookConfigurationSubscriberV1<S> {
    store: S,
}

impl<S> HookConfigurationSubscriberV1<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S> HookConfigurationSubscriberV1<S>
where
    S: HookConfigurationReadStoreV1,
{
    pub fn load_current(&self, host: HookHostV1, now: UtcMicros) -> HookConfigurationReadOutcomeV1 {
        let snapshot = match self.store.load(host) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return HookConfigurationReadOutcomeV1::Missing,
            Err(HookConfigurationPublicationError::Corrupted)
            | Err(HookConfigurationPublicationError::InvalidSnapshot) => {
                return HookConfigurationReadOutcomeV1::Corrupted;
            }
            Err(HookConfigurationPublicationError::Unavailable) => {
                return HookConfigurationReadOutcomeV1::Unavailable;
            }
        };
        if snapshot.validate().is_err() || snapshot.binding.host != host {
            return HookConfigurationReadOutcomeV1::Corrupted;
        }
        if now.0 >= snapshot.expires_at.0 {
            return HookConfigurationReadOutcomeV1::Stale;
        }
        HookConfigurationReadOutcomeV1::Bound(snapshot)
    }
}

/// Daemon-writable endpoint for one profile hook-config JSON path.
#[derive(Clone, Debug)]
pub struct HookConfigurationFileWriterV1 {
    path: PathBuf,
}

impl HookConfigurationFileWriterV1 {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn reader(&self) -> HookConfigurationFileReaderV1 {
        HookConfigurationFileReaderV1::new(self.path.clone())
    }
}

impl HookConfigurationPublicationStoreV1 for HookConfigurationFileWriterV1 {
    fn publish(
        &self,
        snapshot: HookConfigurationSnapshotV1,
    ) -> Result<HookConfigurationPublicationOutcomeV1, HookConfigurationPublicationError> {
        snapshot.validate()?;
        let current = match read_snapshot(&self.path) {
            Ok(current) => current,
            // This writer is the sole daemon-owned publication authority. A
            // structurally stale or malformed prior snapshot cannot authorize
            // anything, but it also must not permanently prevent the authority
            // from replacing it with a validated current snapshot.
            Err(HookConfigurationPublicationError::Corrupted)
            | Err(HookConfigurationPublicationError::InvalidSnapshot) => None,
            Err(error) => return Err(error),
        };
        if let Some(current) = current {
            if current == snapshot {
                return Ok(HookConfigurationPublicationOutcomeV1::Duplicate);
            }
            if current.revision >= snapshot.revision {
                return Ok(HookConfigurationPublicationOutcomeV1::StaleRejected);
            }
        }
        let bytes = serde_json::to_vec(&snapshot)
            .map_err(|_| HookConfigurationPublicationError::InvalidSnapshot)?;
        if bytes.is_empty() || bytes.len() > MAX_HOOK_CONFIGURATION_BYTES {
            return Err(HookConfigurationPublicationError::InvalidSnapshot);
        }
        atomic_write(&self.path, "hook-config", &bytes, DIRECTORY_SYNC_POLICY)
            .map_err(|_| HookConfigurationPublicationError::Unavailable)?;
        Ok(HookConfigurationPublicationOutcomeV1::Published)
    }
}

/// Hook-readable endpoint. It intentionally has no publication method.
#[derive(Clone, Debug)]
pub struct HookConfigurationFileReaderV1 {
    path: PathBuf,
}

impl HookConfigurationFileReaderV1 {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl HookConfigurationReadStoreV1 for HookConfigurationFileReaderV1 {
    fn load(
        &self,
        _host: HookHostV1,
    ) -> Result<Option<HookConfigurationSnapshotV1>, HookConfigurationPublicationError> {
        read_snapshot(&self.path)
    }
}

fn read_snapshot(
    path: &Path,
) -> Result<Option<HookConfigurationSnapshotV1>, HookConfigurationPublicationError> {
    let bytes = match read_bounded(path, MAX_HOOK_CONFIGURATION_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            return Err(HookConfigurationPublicationError::Corrupted);
        }
        Err(_) => return Err(HookConfigurationPublicationError::Unavailable),
    };
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let snapshot = serde_json::from_slice::<HookConfigurationSnapshotV1>(&bytes)
        .map_err(|_| HookConfigurationPublicationError::Corrupted)?;
    snapshot
        .validate()
        .map_err(|_| HookConfigurationPublicationError::Corrupted)?;
    Ok(Some(snapshot))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::{HookCapabilityV1, HookEventFamily, HookEventSupportV1};

    #[derive(Clone, Default)]
    struct Store(Arc<Mutex<Option<HookConfigurationSnapshotV1>>>);

    impl HookConfigurationPublicationStoreV1 for Store {
        fn publish(
            &self,
            snapshot: HookConfigurationSnapshotV1,
        ) -> Result<HookConfigurationPublicationOutcomeV1, HookConfigurationPublicationError>
        {
            let mut current = self.0.lock().unwrap();
            match current.as_ref() {
                Some(existing) if existing.revision > snapshot.revision => {
                    Ok(HookConfigurationPublicationOutcomeV1::StaleRejected)
                }
                Some(existing) if existing == &snapshot => {
                    Ok(HookConfigurationPublicationOutcomeV1::Duplicate)
                }
                Some(existing) if existing.revision == snapshot.revision => {
                    Ok(HookConfigurationPublicationOutcomeV1::StaleRejected)
                }
                _ => {
                    *current = Some(snapshot);
                    Ok(HookConfigurationPublicationOutcomeV1::Published)
                }
            }
        }
    }

    impl HookConfigurationReadStoreV1 for Store {
        fn load(
            &self,
            _host: HookHostV1,
        ) -> Result<Option<HookConfigurationSnapshotV1>, HookConfigurationPublicationError>
        {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "tracedecay-hook-config-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn snapshot(revision: u64, expires_at: i64) -> HookConfigurationSnapshotV1 {
        HookConfigurationSnapshotV1 {
            schema_version: HOOK_CONFIGURATION_SCHEMA_VERSION,
            revision,
            published_at: UtcMicros(1),
            expires_at: UtcMicros(expires_at),
            binding: HookScopeBindingV1 {
                host: HookHostV1::ClaudeCode,
                project_id: [1; 16],
                repository_id: [2; 16],
                worktree_id: [3; 16],
                worktree_epoch: 1,
                binding_token: [4; 32],
                capabilities: vec![HookCapabilityV1 {
                    family: HookEventFamily::SessionBoundary,
                    support: HookEventSupportV1::Native,
                }],
            },
        }
    }

    #[test]
    fn publication_replay_rejects_stale_revision_and_preserves_exact_scope() {
        let store = Store::default();
        let publisher = HookConfigurationPublisherV1::new(store.clone());
        let published = snapshot(2, 100);
        assert_eq!(
            publisher.publish(published.clone()).unwrap(),
            HookConfigurationPublicationOutcomeV1::Published
        );
        assert_eq!(
            publisher.publish(published.clone()).unwrap(),
            HookConfigurationPublicationOutcomeV1::Duplicate
        );
        assert_eq!(
            publisher.publish(snapshot(1, 100)).unwrap(),
            HookConfigurationPublicationOutcomeV1::StaleRejected
        );
        assert_eq!(
            publisher.publish(snapshot(2, 101)).unwrap(),
            HookConfigurationPublicationOutcomeV1::StaleRejected
        );
        let restarted_subscriber = HookConfigurationSubscriberV1::new(store);
        assert_eq!(
            restarted_subscriber.load_current(HookHostV1::ClaudeCode, UtcMicros(2)),
            HookConfigurationReadOutcomeV1::Bound(published)
        );
    }

    #[test]
    fn schema_revision_expiry_and_scope_validation_fail_closed() {
        let store = Store::default();
        let publisher = HookConfigurationPublisherV1::new(store.clone());
        let mut invalid_schema = snapshot(1, 100);
        invalid_schema.schema_version += 1;
        assert_eq!(
            publisher.publish(invalid_schema),
            Err(HookConfigurationPublicationError::InvalidSnapshot)
        );
        assert!(store.0.lock().unwrap().is_none());

        assert_eq!(
            publisher.publish(snapshot(0, 100)),
            Err(HookConfigurationPublicationError::InvalidSnapshot)
        );
        let subscriber = HookConfigurationSubscriberV1::new(store.clone());
        *store.0.lock().unwrap() = Some(snapshot(1, 2));
        assert_eq!(
            subscriber.load_current(HookHostV1::ClaudeCode, UtcMicros(2)),
            HookConfigurationReadOutcomeV1::Stale
        );

        *store.0.lock().unwrap() = Some(snapshot(1, 100));
        assert_eq!(
            subscriber.load_current(HookHostV1::Codex, UtcMicros(2)),
            HookConfigurationReadOutcomeV1::Corrupted
        );
    }

    #[test]
    fn file_store_is_private_atomic_plain_json_with_bounded_decode() {
        let directory = TestDir::new();
        let path = directory.path.join("hook-config.json");
        let writer = HookConfigurationFileWriterV1::new(&path);
        let reader = writer.reader();
        let published = snapshot(2, 100);
        assert_eq!(
            HookConfigurationPublisherV1::new(writer.clone())
                .publish(published.clone())
                .unwrap(),
            HookConfigurationPublicationOutcomeV1::Published
        );
        let value = serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["revision"], 2);
        assert_eq!(
            HookConfigurationSubscriberV1::new(reader.clone())
                .load_current(HookHostV1::ClaudeCode, UtcMicros(2)),
            HookConfigurationReadOutcomeV1::Bound(published)
        );
        assert_eq!(
            HookConfigurationPublisherV1::new(writer)
                .publish(snapshot(1, 100))
                .unwrap(),
            HookConfigurationPublicationOutcomeV1::StaleRejected
        );
        let entries = fs::read_dir(&directory.path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![std::ffi::OsString::from("hook-config.json")]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        fs::write(&path, vec![b'x'; MAX_HOOK_CONFIGURATION_BYTES + 1]).unwrap();
        assert_eq!(
            HookConfigurationSubscriberV1::new(reader)
                .load_current(HookHostV1::ClaudeCode, UtcMicros(2)),
            HookConfigurationReadOutcomeV1::Corrupted
        );
    }
}
