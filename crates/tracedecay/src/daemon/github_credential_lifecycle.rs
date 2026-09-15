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

#[derive(Clone)]
pub(super) struct DaemonGitHubReadOnlyCredentialLifecycleV1 {
    lifecycle: GitHubReadOnlyCredentialLifecycleV1,
    secrets: Arc<dyn GitHubSecretReadPortV1>,
}

impl Default for DaemonGitHubReadOnlyCredentialLifecycleV1 {
    fn default() -> Self {
        Self::new(
            GitHubReadOnlyCredentialLifecycleV1::default(),
            Arc::new(ProductionOsSecretReadPortV1),
        )
    }
}

impl DaemonGitHubReadOnlyCredentialLifecycleV1 {
    fn new(
        lifecycle: GitHubReadOnlyCredentialLifecycleV1,
        secrets: Arc<dyn GitHubSecretReadPortV1>,
    ) -> Self {
        Self { lifecycle, secrets }
    }

    pub(super) fn mount(
        &self,
        profile_id: &UserProfileId,
        repository_owner: &str,
        repository_name: &str,
    ) -> ProfileGitHubReadOnlyCredentialMountOutcomeV1 {
        self.lifecycle
            .mount(profile_id, repository_owner, repository_name)
    }

    pub(super) fn configure_profile(
        &self,
        identity: &profile_identity::LocalProfileIdentityAuthorityV1,
    ) {
        self.lifecycle.configure_profile(
            identity.profile_id(),
            identity.profile_root(),
            Arc::clone(&self.secrets),
        );
    }

    pub(super) fn shutdown(&self) {
        self.lifecycle.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    use tracedecay_application::advisory::github_runtime::{
        GitHubReadOnlyCredentialAuthorityOutcomeV1, GitHubReadOnlyCredentialPermissionVerifierV1,
        GitHubReadOnlyCredentialSecretV1, GitHubReadPermissionV1,
        RegisteredGitHubReadOnlyCredentialV1, resolve_registered_github_read_only_credential_v1,
    };

    use super::*;
    use crate::daemon::DaemonInvocationState;

    struct RecordingSecretReadV1 {
        calls: Mutex<Vec<(String, String)>>,
    }

    impl GitHubSecretReadPortV1 for RecordingSecretReadV1 {
        fn read_secret(
            &self,
            service: &str,
            account: &str,
        ) -> Result<Option<Zeroizing<String>>, GitHubSecretReadErrorV1> {
            self.calls
                .lock()
                .expect("secret-read calls")
                .push((service.to_owned(), account.to_owned()));
            Ok(Some(Zeroizing::new("github-read-token".to_owned())))
        }
    }

    struct ReadOnlyPermissionVerifierV1;

    impl GitHubReadOnlyCredentialPermissionVerifierV1 for ReadOnlyPermissionVerifierV1 {
        fn verify(
            &self,
            secret: &str,
            repository_owner: &str,
        ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
            assert_eq!(secret, "github-read-token");
            assert_eq!(repository_owner, "ScriptedAlchemy");
            GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified {
                secret: GitHubReadOnlyCredentialSecretV1::from_zeroizing(Zeroizing::new(
                    secret.to_owned(),
                ))
                .expect("non-empty credential"),
                exact_permissions: BTreeSet::from([
                    GitHubReadPermissionV1::Metadata,
                    GitHubReadPermissionV1::PullRequests,
                ]),
            }
        }
    }

    fn invocation_with_credentials(secrets: Arc<RecordingSecretReadV1>) -> DaemonInvocationState {
        let lifecycle = GitHubReadOnlyCredentialLifecycleV1::with_permission_verifier(Arc::new(
            ReadOnlyPermissionVerifierV1,
        ));
        let mut invocation = DaemonInvocationState::default();
        invocation.github_credential_lifecycle =
            DaemonGitHubReadOnlyCredentialLifecycleV1::new(lifecycle, secrets);
        invocation
    }

    fn assert_verified() {
        assert!(matches!(
            resolve_registered_github_read_only_credential_v1(
                "ScriptedAlchemy",
                "private-repository"
            ),
            RegisteredGitHubReadOnlyCredentialV1::Verified(_)
        ));
    }

    fn assert_cleared() {
        assert!(matches!(
            resolve_registered_github_read_only_credential_v1(
                "ScriptedAlchemy",
                "private-repository"
            ),
            RegisteredGitHubReadOnlyCredentialV1::Missing
        ));
    }

    #[tokio::test]
    async fn daemon_identity_secret_injection_and_shutdown_paths_clear_registration_across_restart()
    {
        let temporary = tempfile::tempdir().expect("profile root");
        let profile_root = temporary.path().join("profile");
        let identity =
            profile_identity::load_or_create(&profile_root).expect("profile identity authority");
        std::fs::write(
            identity.profile_root().join("config.toml"),
            r#"
[[github_review_sources]]
owner = "ScriptedAlchemy"
repository = "private-repository"
access = "os_keyring"
keyring_service = "tracedecay.github"
keyring_account = "review"
"#,
        )
        .expect("credential configuration");
        let secrets = Arc::new(RecordingSecretReadV1 {
            calls: Mutex::new(Vec::new()),
        });

        let shutdown_generation = invocation_with_credentials(Arc::clone(&secrets));
        shutdown_generation.configure_github_read_only_credentials(&identity);
        assert_eq!(
            shutdown_generation.mount_github_read_only_credential_authority_for_project(
                identity.profile_id(),
                "ScriptedAlchemy",
                "private-repository",
            ),
            ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted
        );
        assert_verified();
        assert!(shutdown_generation.shutdown().await.is_clean());
        assert_cleared();

        let cancelled_generation = invocation_with_credentials(Arc::clone(&secrets));
        cancelled_generation.configure_github_read_only_credentials(&identity);
        assert_eq!(
            cancelled_generation.mount_github_read_only_credential_authority_for_project(
                identity.profile_id(),
                "ScriptedAlchemy",
                "private-repository",
            ),
            ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted
        );
        assert_verified();
        cancelled_generation.cancel_admissions();
        assert_cleared();

        let restarted_generation = invocation_with_credentials(Arc::clone(&secrets));
        restarted_generation.configure_github_read_only_credentials(&identity);
        assert_eq!(
            restarted_generation.mount_github_read_only_credential_authority_for_project(
                identity.profile_id(),
                "ScriptedAlchemy",
                "private-repository",
            ),
            ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted
        );
        assert_verified();
        restarted_generation.cancel_admissions();
        assert_cleared();

        assert_eq!(
            secrets.calls.lock().expect("secret-read calls").as_slice(),
            &[
                ("tracedecay.github".to_owned(), "review".to_owned()),
                ("tracedecay.github".to_owned(), "review".to_owned()),
                ("tracedecay.github".to_owned(), "review".to_owned()),
            ]
        );
    }
}
