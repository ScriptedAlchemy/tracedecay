use std::sync::Arc;

use tracedecay_application::advisory::github_runtime::{
    GitHubReadOnlyCredentialLifecycleV1, GitHubSecretReadErrorV1, GitHubSecretReadPortV1,
    ProfileGitHubReadOnlyCredentialMountOutcomeV1,
};
use tracedecay_daemon_identity::profile_identity;
use tracedecay_domain::UserProfileId;
use zeroize::Zeroizing;

struct ProductionOsSecretReadPortV1;

impl GitHubSecretReadPortV1 for ProductionOsSecretReadPortV1 {
    fn read_secret(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<String>>, GitHubSecretReadErrorV1> {
        let entry = keyring::v1::Entry::new(service, account)
            .map_err(|_| GitHubSecretReadErrorV1::Unavailable)?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::v1::Error::NoEntry) => Ok(None),
            Err(_) => Err(GitHubSecretReadErrorV1::Unavailable),
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct DaemonGitHubReadOnlyCredentialLifecycleV1(GitHubReadOnlyCredentialLifecycleV1);

impl DaemonGitHubReadOnlyCredentialLifecycleV1 {
    pub(super) fn mount(
        &self,
        profile_id: &UserProfileId,
        repository_owner: &str,
        repository_name: &str,
    ) -> ProfileGitHubReadOnlyCredentialMountOutcomeV1 {
        self.0.mount(profile_id, repository_owner, repository_name)
    }

    pub(super) fn configure_profile(
        &self,
        identity: &profile_identity::LocalProfileIdentityAuthorityV1,
    ) {
        self.0.configure_profile(
            identity.profile_id(),
            identity.profile_root(),
            Arc::new(ProductionOsSecretReadPortV1),
        );
    }

    pub(super) fn shutdown(&self) {
        self.0.shutdown();
    }
}
