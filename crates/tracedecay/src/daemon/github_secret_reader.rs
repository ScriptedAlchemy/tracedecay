use tracedecay_application::advisory::github_runtime::{
    GitHubSecretReadErrorV1, GitHubSecretReadPortV1,
};
use zeroize::Zeroizing;

pub(super) struct ProductionOsSecretReadPortV1;

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
