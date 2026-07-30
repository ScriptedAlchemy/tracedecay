//! Daemon-owned trusted provisioning boundary for Remote enrollment grants.

use tracedecay_application::remote::auth::RemoteEnrollmentAuthorityErrorV1;
use tracedecay_domain::EnrollmentGrantV1;
use tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle;
use tracedecay_rusqlite_runtime::remote_authority::RegisteredRemoteEnrollmentAuthorityV1;

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

    /// Installs a fingerprint-only grant supplied by trusted local
    /// configuration or administration. Protocol enrollment requests have no
    /// path to this method and therefore cannot mint their own grants.
    pub(crate) fn provision_grant(
        &self,
        grant: &EnrollmentGrantV1,
    ) -> Result<(), RemoteEnrollmentAuthorityErrorV1> {
        self.authority.provision_grant(grant)
    }

    pub(crate) fn authority(&self) -> RegisteredRemoteEnrollmentAuthorityV1 {
        self.authority.clone()
    }
}
