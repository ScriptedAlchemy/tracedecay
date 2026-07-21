//! Cross-process Hook V2 configuration publication contracts.
//!
//! A daemon-owned authority signs/verifies and atomically publishes these
//! compact bindings. Hook processes only load a verified, exact-host binding;
//! they never discover a project from a path or open a TraceDecay store.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::UtcMicros;

use crate::{HookHostV1, HookScopeBindingV1};

pub const HOOK_CONFIGURATION_SCHEMA_VERSION: u16 = 1;
pub const MAX_HOOK_CONFIGURATION_INTEGRITY_BYTES: usize = 512;

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

/// Opaque integrity material is verified by an injected daemon/configuration
/// authority. Hook code never receives a signing key or implements a trust
/// root locally.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookConfigurationPublicationV1 {
    pub snapshot: HookConfigurationSnapshotV1,
    pub integrity_tag: Vec<u8>,
}

impl HookConfigurationPublicationV1 {
    pub fn validate_structure(&self) -> Result<(), HookConfigurationPublicationError> {
        self.snapshot.validate()?;
        if self.integrity_tag.is_empty()
            || self.integrity_tag.len() > MAX_HOOK_CONFIGURATION_INTEGRITY_BYTES
        {
            return Err(HookConfigurationPublicationError::InvalidSnapshot);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookConfigurationPublicationOutcomeV1 {
    Published,
    Duplicate,
    Superseded,
}

/// Result exposed to a hook process. The states are intentionally content-free
/// and do not disclose a different host's binding or configuration existence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookConfigurationReadOutcomeV1 {
    Bound(HookScopeBindingV1),
    Missing,
    Stale,
    Corrupted,
    Unavailable,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HookConfigurationPublicationError {
    #[error("hook configuration snapshot is structurally invalid")]
    InvalidSnapshot,
    #[error("hook configuration integrity verification failed")]
    IntegrityRejected,
    #[error("hook configuration publication authority is unavailable")]
    Unavailable,
}

/// Injected integrity verifier. The daemon/configuration owner selects its
/// concrete signed-publication mechanism; hooks receive no crypto authority.
pub trait HookConfigurationIntegrityVerifierV1 {
    fn verify(
        &self,
        publication: &HookConfigurationPublicationV1,
    ) -> Result<(), HookConfigurationPublicationError>;
}

/// Atomic cross-process publication seam. An implementation may use a signed
/// daemon IPC response, an atomic file replacement, or another authorized
/// transport, but that filesystem/IPC choice stays outside this crate.
pub trait HookConfigurationPublicationStoreV1 {
    fn publish(
        &self,
        publication: HookConfigurationPublicationV1,
    ) -> Result<HookConfigurationPublicationOutcomeV1, HookConfigurationPublicationError>;

    fn load(
        &self,
        host: HookHostV1,
    ) -> Result<Option<HookConfigurationPublicationV1>, HookConfigurationPublicationError>;
}

/// Publishing adapter that verifies structure and injected integrity before an
/// external store sees a configuration record.
pub struct HookConfigurationPublisherV1<S, V> {
    store: S,
    verifier: V,
}

impl<S, V> HookConfigurationPublisherV1<S, V> {
    pub fn new(store: S, verifier: V) -> Self {
        Self { store, verifier }
    }
}

impl<S, V> HookConfigurationPublisherV1<S, V>
where
    S: HookConfigurationPublicationStoreV1,
    V: HookConfigurationIntegrityVerifierV1,
{
    pub fn publish(
        &self,
        publication: HookConfigurationPublicationV1,
    ) -> Result<HookConfigurationPublicationOutcomeV1, HookConfigurationPublicationError> {
        publication.validate_structure()?;
        self.verifier.verify(&publication)?;
        self.store.publish(publication)
    }
}

/// Subscriber adapter for a separate hook process. It revalidates both the
/// atomic record and injected integrity on every read, then rejects expiry
/// before exposing the binding to a native decoder.
pub struct HookConfigurationSubscriberV1<S, V> {
    store: S,
    verifier: V,
}

impl<S, V> HookConfigurationSubscriberV1<S, V> {
    pub fn new(store: S, verifier: V) -> Self {
        Self { store, verifier }
    }
}

impl<S, V> HookConfigurationSubscriberV1<S, V>
where
    S: HookConfigurationPublicationStoreV1,
    V: HookConfigurationIntegrityVerifierV1,
{
    pub fn load_current(&self, host: HookHostV1, now: UtcMicros) -> HookConfigurationReadOutcomeV1 {
        let publication = match self.store.load(host) {
            Ok(Some(publication)) => publication,
            Ok(None) => return HookConfigurationReadOutcomeV1::Missing,
            Err(_) => return HookConfigurationReadOutcomeV1::Unavailable,
        };
        if publication.validate_structure().is_err()
            || publication.snapshot.binding.host != host
            || self.verifier.verify(&publication).is_err()
        {
            return HookConfigurationReadOutcomeV1::Corrupted;
        }
        if now.0 >= publication.snapshot.expires_at.0 {
            return HookConfigurationReadOutcomeV1::Stale;
        }
        HookConfigurationReadOutcomeV1::Bound(publication.snapshot.binding)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;

    use super::*;
    use crate::{HookCapabilityV1, HookEventFamily, HookEventSupportV1};

    #[derive(Clone, Default)]
    struct Store(Arc<Mutex<Option<HookConfigurationPublicationV1>>>);

    impl HookConfigurationPublicationStoreV1 for Store {
        fn publish(
            &self,
            publication: HookConfigurationPublicationV1,
        ) -> Result<HookConfigurationPublicationOutcomeV1, HookConfigurationPublicationError>
        {
            let mut current = self.0.lock().unwrap();
            match current.as_ref() {
                Some(existing) if existing.snapshot.revision > publication.snapshot.revision => {
                    Ok(HookConfigurationPublicationOutcomeV1::Superseded)
                }
                Some(existing) if existing == &publication => {
                    Ok(HookConfigurationPublicationOutcomeV1::Duplicate)
                }
                _ => {
                    *current = Some(publication);
                    Ok(HookConfigurationPublicationOutcomeV1::Published)
                }
            }
        }

        fn load(
            &self,
            host: HookHostV1,
        ) -> Result<Option<HookConfigurationPublicationV1>, HookConfigurationPublicationError>
        {
            Ok(self
                .0
                .lock()
                .unwrap()
                .as_ref()
                .filter(|publication| publication.snapshot.binding.host == host)
                .cloned())
        }
    }

    #[derive(Clone, Copy)]
    struct Verifier;

    impl HookConfigurationIntegrityVerifierV1 for Verifier {
        fn verify(
            &self,
            publication: &HookConfigurationPublicationV1,
        ) -> Result<(), HookConfigurationPublicationError> {
            (publication.integrity_tag.as_slice() == b"verified")
                .then_some(())
                .ok_or(HookConfigurationPublicationError::IntegrityRejected)
        }
    }

    fn publication(expires_at: i64) -> HookConfigurationPublicationV1 {
        HookConfigurationPublicationV1 {
            snapshot: HookConfigurationSnapshotV1 {
                schema_version: HOOK_CONFIGURATION_SCHEMA_VERSION,
                revision: 1,
                published_at: UtcMicros(1),
                expires_at: UtcMicros(expires_at),
                binding: HookScopeBindingV1 {
                    host: HookHostV1::ClaudeCode,
                    project_id: [1; 16],
                    repository_id: [2; 16],
                    worktree_id: [3; 16],
                    worktree_epoch: 1,
                    authorization_epoch: 1,
                    capability_revision: 1,
                    binding_token: [4; 32],
                    capabilities: vec![HookCapabilityV1 {
                        family: HookEventFamily::SessionBoundary,
                        support: HookEventSupportV1::Native,
                    }],
                },
            },
            integrity_tag: b"verified".to_vec(),
        }
    }

    #[test]
    fn publication_replay_is_atomic_and_new_subscriber_loads_the_same_binding() {
        let store = Store::default();
        let publisher = HookConfigurationPublisherV1::new(store.clone(), Verifier);
        let published = publication(100);
        assert_eq!(
            publisher.publish(published.clone()).unwrap(),
            HookConfigurationPublicationOutcomeV1::Published
        );
        assert_eq!(
            publisher.publish(published.clone()).unwrap(),
            HookConfigurationPublicationOutcomeV1::Duplicate
        );
        let restarted_subscriber = HookConfigurationSubscriberV1::new(store, Verifier);
        assert_eq!(
            restarted_subscriber.load_current(HookHostV1::ClaudeCode, UtcMicros(2)),
            HookConfigurationReadOutcomeV1::Bound(published.snapshot.binding)
        );
    }

    #[test]
    fn corruption_and_expiry_never_publish_a_binding_to_a_hook_process() {
        let store = Store::default();
        let publisher = HookConfigurationPublisherV1::new(store.clone(), Verifier);
        let mut corrupt = publication(100);
        corrupt.integrity_tag = b"tampered".to_vec();
        assert_eq!(
            publisher.publish(corrupt),
            Err(HookConfigurationPublicationError::IntegrityRejected)
        );
        assert!(store.0.lock().unwrap().is_none());

        *store.0.lock().unwrap() = Some(HookConfigurationPublicationV1 {
            integrity_tag: b"tampered".to_vec(),
            ..publication(100)
        });
        let subscriber = HookConfigurationSubscriberV1::new(store.clone(), Verifier);
        assert_eq!(
            subscriber.load_current(HookHostV1::ClaudeCode, UtcMicros(2)),
            HookConfigurationReadOutcomeV1::Corrupted
        );

        *store.0.lock().unwrap() = Some(publication(2));
        assert_eq!(
            subscriber.load_current(HookHostV1::ClaudeCode, UtcMicros(2)),
            HookConfigurationReadOutcomeV1::Stale
        );
    }
}
