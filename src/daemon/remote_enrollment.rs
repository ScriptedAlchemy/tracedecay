//! Daemon-owned trusted provisioning boundary for Remote enrollment grants.

use std::sync::Arc;
use std::{io::Read, path::Path};

use serde::Deserialize;
use thiserror::Error;
use tracedecay_application::remote::auth::{
    RemoteEnrollmentAdmissionEvidenceV1, RemoteEnrollmentAuthorityErrorV1,
    RemoteEnrollmentProtocolAdapterV1,
};
use tracedecay_application::remote::protocol::RemoteEnrollmentProtocolPortV1;
use tracedecay_domain::EnrollmentGrantV1;
use tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle;
use tracedecay_rusqlite_runtime::remote_authority::RegisteredRemoteEnrollmentAuthorityV1;

const MAX_REMOTE_ENROLLMENT_CONFIG_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfiguredRemoteEnrollmentGrantV1 {
    grant: EnrollmentGrantV1,
    admission: RemoteEnrollmentAdmissionEvidenceV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteEnrollmentProvisioningConfigV1 {
    grants: Vec<ConfiguredRemoteEnrollmentGrantV1>,
}

#[derive(Debug, Error)]
pub(crate) enum DaemonRemoteEnrollmentProvisioningErrorV1 {
    #[error("remote enrollment provisioning configuration is unavailable")]
    ConfigurationUnavailable,
    #[error("remote enrollment provisioning configuration is invalid")]
    InvalidConfiguration,
    #[error(transparent)]
    Authority(#[from] RemoteEnrollmentAuthorityErrorV1),
}

#[derive(Clone)]
pub(crate) struct DaemonRemoteEnrollmentProvisionerV1 {
    authority: RegisteredRemoteEnrollmentAuthorityV1,
}

impl DaemonRemoteEnrollmentProvisionerV1 {
    pub(crate) fn from_registered(
        handle: MigrationSqlHandle,
    ) -> Result<Self, RemoteEnrollmentAuthorityErrorV1> {
        Ok(Self {
            authority: RegisteredRemoteEnrollmentAuthorityV1::from_registered(handle)?,
        })
    }

    pub(crate) fn from_registered_configured(
        handle: MigrationSqlHandle,
        configuration_path: &Path,
    ) -> Result<Self, DaemonRemoteEnrollmentProvisioningErrorV1> {
        let provisioner = Self::from_registered(handle)?;
        provisioner.provision_from_configuration(configuration_path)?;
        Ok(provisioner)
    }

    /// Installs a fingerprint-only grant supplied by trusted local
    /// configuration or administration. Protocol enrollment requests have no
    /// path to this method and therefore cannot mint their own grants.
    pub(crate) fn provision_grant(
        &self,
        grant: &EnrollmentGrantV1,
        admission: &RemoteEnrollmentAdmissionEvidenceV1,
    ) -> Result<(), RemoteEnrollmentAuthorityErrorV1> {
        self.authority.provision_grant(grant, admission)
    }

    fn provision_from_configuration(
        &self,
        path: &Path,
    ) -> Result<(), DaemonRemoteEnrollmentProvisioningErrorV1> {
        let file = std::fs::File::open(path)
            .map_err(|_| DaemonRemoteEnrollmentProvisioningErrorV1::ConfigurationUnavailable)?;
        let mut bytes = Vec::with_capacity(MAX_REMOTE_ENROLLMENT_CONFIG_BYTES as usize);
        file.take(MAX_REMOTE_ENROLLMENT_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| DaemonRemoteEnrollmentProvisioningErrorV1::ConfigurationUnavailable)?;
        if bytes.len() as u64 > MAX_REMOTE_ENROLLMENT_CONFIG_BYTES {
            return Err(DaemonRemoteEnrollmentProvisioningErrorV1::InvalidConfiguration);
        }
        let configuration: RemoteEnrollmentProvisioningConfigV1 = serde_json::from_slice(&bytes)
            .map_err(|_| DaemonRemoteEnrollmentProvisioningErrorV1::InvalidConfiguration)?;
        for configured in configuration.grants {
            self.provision_grant(&configured.grant, &configured.admission)?;
        }
        Ok(())
    }

    pub(crate) fn protocol_port(&self) -> Arc<dyn RemoteEnrollmentProtocolPortV1> {
        Arc::new(RemoteEnrollmentProtocolAdapterV1::new(
            self.authority.clone(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use rusqlite::Savepoint;
    use tempfile::TempDir;
    use tracedecay_application::remote::auth::{
        OpaqueRemoteCredential, RemoteEnrollmentAdmissionEvidenceV1,
        RemoteEnrollmentAuthorityPortV1,
    };
    use tracedecay_application::remote::protocol::{EnrollmentRequestV1, RemoteProtocolRequestV1};
    use tracedecay_application::{
        AuthorityReceipt, CapabilityGrantId, Deadline, DisclosureClass, PolicyDecisionRef,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::{
        ActorId, BrainId, BrainNodeId, ComponentVersion, EntityId, LocatorDigest, ManifestDigest,
        ProjectId, RefId, RemoteCapabilityV1, RemoteCredentialFingerprintV1,
        RemoteRepositoryScopeV1, RepositoryId, RepositoryStateSnapshotId, UtcMicros, WorktreeId,
        canonical_sha256,
    };
    use tracedecay_rusqlite_runtime::reader::{
        ExistingReaderLocator, ReaderPool, ReaderQueryExecutor,
    };
    use tracedecay_rusqlite_runtime::{
        ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    };
    use tracedecay_store::{
        AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
        StorageRuntimeErrorV1, StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
    };

    use super::*;

    struct NoTypedWrites;

    impl StorageOperationExecutor for NoTypedWrites {
        fn execute(
            &mut self,
            _savepoint: &Savepoint<'_>,
            _payload: &RepositoryWritePayloadV1,
        ) -> rusqlite::Result<()> {
            unreachable!("enrollment uses only the registered migration-SQL channel")
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
            unreachable!("enrollment uses only the registered migration-SQL channel")
        }
    }

    struct RegisteredStore {
        handle: MigrationSqlHandle,
        path: PathBuf,
        writer: PersistentWriter,
        readers: ReaderPool<NoTypedReads>,
        directory: TempDir,
    }

    impl RegisteredStore {
        fn start() -> Self {
            let directory = TempDir::new().unwrap();
            let path = directory.path().join("remote-enrollment-config.sqlite3");
            rusqlite::Connection::open(&path).unwrap();
            Self::open(path.canonicalize().unwrap(), directory)
        }

        fn open(path: PathBuf, directory: TempDir) -> Self {
            let binding: StoreRuntimeBindingV1 = serde_json::from_value(serde_json::json!({
                "shard_id": {
                    "brain_id": "brain.remote",
                    "profile_id": "profile.remote",
                    "scope": { "kind": "project", "project_id": "project.remote" }
                },
                "incarnation": 1,
                "authority_epoch": 1
            }))
            .unwrap();
            let locator = VerifiedStoreLocatorV1::new(
                binding.shard_id.clone(),
                StoreIncarnationV1::new(1).unwrap(),
                LocatorDigest::new(format!("sha256:{}", "5".repeat(64))).unwrap(),
            );
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
            let handle = MigrationSqlHandle::attach(&writer, &readers).unwrap();
            Self {
                handle,
                path,
                writer,
                readers,
                directory,
            }
        }

        fn restart(self) -> Self {
            let Self {
                handle,
                path,
                writer,
                readers,
                directory,
            } = self;
            drop(handle);
            drop(readers);
            drop(writer);
            Self::open(path, directory)
        }
    }

    fn credential(byte: u8) -> OpaqueRemoteCredential {
        OpaqueRemoteCredential::new(vec![byte; 32].into_boxed_slice()).unwrap()
    }

    fn scope() -> RemoteRepositoryScopeV1 {
        RemoteRepositoryScopeV1 {
            project_id: ProjectId::new("project.remote").unwrap(),
            repository_id: RepositoryId::new("repository.remote").unwrap(),
            worktree_id: WorktreeId::new("worktree.remote").unwrap(),
            reference: Some(RefId::new("refs/heads/main").unwrap()),
            snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote").unwrap(),
        }
    }

    fn configured_grant() -> (EnrollmentGrantV1, RemoteEnrollmentAdmissionEvidenceV1) {
        let grant = EnrollmentGrantV1 {
            grant_id: EntityId::new("grant.remote").unwrap(),
            brain_id: BrainId::new("brain.remote").unwrap(),
            node_id: BrainNodeId::new("node.remote").unwrap(),
            fingerprint: RemoteCredentialFingerprintV1::from_secret(&[b'g'; 32]).unwrap(),
            revision: 1,
            issued_at: UtcMicros(1),
            expires_at: UtcMicros(100),
            revoked_at: None,
            capabilities: BTreeSet::from([RemoteCapabilityV1::Query]),
            scope: scope(),
        };
        let scope = ResolvedScope::new(
            grant.scope.project_id.clone(),
            grant.scope.repository_id.clone(),
            grant.scope.worktree_id.clone(),
            grant.scope.reference.clone(),
        )
        .unwrap();
        let digest = canonical_sha256(&grant).unwrap();
        let admission = RemoteEnrollmentAdmissionEvidenceV1::new(
            &grant,
            scope.clone(),
            AuthorityReceipt {
                grant_id: CapabilityGrantId::new(grant.grant_id.as_str()).unwrap(),
                grant_revision: grant.revision,
                grant_digest: digest.clone(),
                authorized_scope_digest: scope.scope_digest,
                disclosure: DisclosureClass::Evidence,
                policy: PolicyDecisionRef::new(
                    "policy.remote.enrollment",
                    1,
                    digest,
                    ComponentVersion::new("policy.remote.enrollment.v1").unwrap(),
                )
                .unwrap(),
                revalidated_at: UtcMicros(10),
            },
            ActorId::new("actor.remote.node").unwrap(),
            ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
            ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
            ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
            Deadline::new(UtcMicros(100)).unwrap(),
        )
        .unwrap();
        (grant, admission)
    }

    #[test]
    fn configured_grant_is_provisioned_before_protocol_and_survives_restart() {
        let store = RegisteredStore::start();
        let grant_credential = credential(b'g');
        let enrollment_credential = credential(b'e');
        let (grant, admission) = configured_grant();
        let config_path = store.directory.path().join("remote-enrollment.json");
        std::fs::write(
            &config_path,
            serde_json::to_vec(&serde_json::json!({
                "grants": [{ "grant": grant.clone(), "admission": admission.clone() }]
            }))
            .unwrap(),
        )
        .unwrap();
        let configured_bytes = std::fs::read(&config_path).unwrap();
        assert!(
            !configured_bytes
                .windows(32)
                .any(|window| window == [b'g'; 32] || window == [b'e'; 32])
        );
        let provisioner = DaemonRemoteEnrollmentProvisionerV1::from_registered_configured(
            store.handle.clone(),
            &config_path,
        )
        .unwrap();
        assert!(provisioner.authority.load_grant(&grant.grant_id).is_ok());

        let brain_id = grant.brain_id.clone();
        let node_id = grant.node_id.clone();
        let response = provisioner.protocol_port().execute_enrollment(
            RemoteProtocolRequestV1::new_initial_enrollment(
                RequestId::new("request.remote.enrollment").unwrap(),
                brain_id.clone(),
                node_id.clone(),
                UtcMicros(10),
                EnrollmentRequestV1 {
                    grant_id: grant.grant_id.clone(),
                    grant_revision: grant.revision,
                    enrollment_id: EntityId::new("enrollment.remote").unwrap(),
                    brain_id,
                    node_id,
                    expires_at: UtcMicros(90),
                    capabilities: grant.capabilities.clone(),
                    scope: grant.scope.clone(),
                },
            )
            .unwrap(),
            grant_credential,
            enrollment_credential,
        );
        assert!(response.result.is_ok());
        drop(provisioner);

        let store = store.restart();
        let reopened = DaemonRemoteEnrollmentProvisionerV1::from_registered_configured(
            store.handle.clone(),
            &config_path,
        )
        .unwrap();
        assert!(reopened.authority.load_grant(&grant.grant_id).is_err());
        assert!(
            reopened
                .authority
                .load_commit_receipt(&EntityId::new("enrollment.remote").unwrap())
                .is_ok()
        );
    }
}
