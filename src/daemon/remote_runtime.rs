use std::io::{ErrorKind, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use tracedecay_rusqlite_runtime::remote_authority::RusqliteRemoteAuthorityStoreV1;
use tracedecay_rusqlite_runtime::repository::{
    RepositoryPhysicalAttachmentFactory, RepositoryRuntimePhysicalAttachment,
};
use tracedecay_store::{
    AdmissionConfigV1, StoreRuntimeBindingV1, StoreShardScopeV1, VerifiedStoreLocatorV1,
};

use super::remote_https::{
    RemoteBrainHttpsConfigV1, RemoteBrainHttpsEnablementV1, RemoteBrainHttpsService,
    RemoteBrainHttpsStateV1,
};
use super::remote_protocol::CanonicalDaemonRemoteProtocolOwnersV1;
use crate::errors::{Result, TraceDecayError};

const REMOTE_RUNTIME_CONFIG_FILE: &str = "remote-brain-runtime.json";
const MAX_REMOTE_RUNTIME_CONFIG_BYTES: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteBrainRuntimeConfigV1 {
    version: u32,
    https_configuration_path: PathBuf,
    enrollment_configuration_path: PathBuf,
    repository_database_path: PathBuf,
    repository_binding: StoreRuntimeBindingV1,
}

impl RemoteBrainRuntimeConfigV1 {
    fn resolve_paths(&mut self, directory: &Path) {
        for path in [
            &mut self.https_configuration_path,
            &mut self.enrollment_configuration_path,
            &mut self.repository_database_path,
        ] {
            if path.is_relative() {
                *path = directory.join(&*path);
            }
        }
    }

    fn validate(&self) -> Result<()> {
        if self.version != 1
            || !matches!(
                self.repository_binding.shard_id.scope,
                StoreShardScopeV1::ProjectSessions { .. }
            )
        {
            return Err(configuration_error(
                "Remote query runtime configuration is invalid",
            ));
        }
        Ok(())
    }
}

pub(crate) enum DaemonRemoteQueryRuntimeV1 {
    Unconfigured,
    Disabled,
    Running {
        listener: RemoteBrainHttpsService,
        repository: Arc<RepositoryRuntimePhysicalAttachment>,
    },
}

impl DaemonRemoteQueryRuntimeV1 {
    pub(crate) fn state(&self) -> RemoteBrainHttpsStateV1 {
        match self {
            Self::Unconfigured => RemoteBrainHttpsStateV1::Unconfigured,
            Self::Disabled => RemoteBrainHttpsStateV1::Disabled,
            Self::Running { .. } => RemoteBrainHttpsStateV1::Degraded,
        }
    }

    pub(crate) fn endpoint(&self) -> Option<SocketAddr> {
        match self {
            Self::Running { listener, .. } => Some(listener.endpoint()),
            Self::Unconfigured | Self::Disabled => None,
        }
    }

    pub(crate) async fn shutdown(self) {
        let Self::Running {
            listener,
            repository,
        } = self
        else {
            return;
        };
        let _ = listener.shutdown().await;
        let _ = repository.drain();
        let _ = repository.close_and_join();
    }
}

pub(crate) async fn start_daemon_remote_query_runtime(
    profile_root: &Path,
) -> Result<DaemonRemoteQueryRuntimeV1> {
    let configuration_path = profile_root.join(REMOTE_RUNTIME_CONFIG_FILE);
    let mut configuration = match load_runtime_configuration(&configuration_path)? {
        Some(configuration) => configuration,
        None => return Ok(DaemonRemoteQueryRuntimeV1::Unconfigured),
    };
    configuration.resolve_paths(
        configuration_path
            .parent()
            .unwrap_or_else(|| Path::new(".")),
    );
    configuration.validate()?;

    let https =
        RemoteBrainHttpsConfigV1::load_optional(Some(&configuration.https_configuration_path))
            .map_err(|error| configuration_error(&error.to_string()))?;
    match https.enablement {
        RemoteBrainHttpsEnablementV1::Unconfigured => {
            return Ok(DaemonRemoteQueryRuntimeV1::Unconfigured);
        }
        RemoteBrainHttpsEnablementV1::Disabled => {
            return Ok(DaemonRemoteQueryRuntimeV1::Disabled);
        }
        RemoteBrainHttpsEnablementV1::Enabled => {}
    }
    https
        .validate_enabled()
        .map_err(|error| configuration_error(&error.to_string()))?;

    let repository_path = configuration
        .repository_database_path
        .canonicalize()
        .map_err(|error| configuration_error(&error.to_string()))?;
    let enrollment_configuration_path = configuration
        .enrollment_configuration_path
        .canonicalize()
        .map_err(|error| configuration_error(&error.to_string()))?;
    let locator_digest =
        super::store_runtime::resolver::canonical_store_locator_digest(&repository_path)
            .map_err(|error| configuration_error(&error))?;
    let locator = VerifiedStoreLocatorV1::new(
        configuration.repository_binding.shard_id.clone(),
        configuration.repository_binding.incarnation,
        locator_digest,
    );
    let repository = Arc::new(
        RepositoryPhysicalAttachmentFactory
            .attach(
                configuration.repository_binding,
                locator,
                repository_path.clone(),
                AdmissionConfigV1::default(),
            )
            .map_err(|error| configuration_error(&error.to_string()))?,
    );
    let enrollment_store = repository
        .migration_sql_handle()
        .map_err(|error| configuration_error(&error.to_string()))?;
    let authority = Arc::new(
        RusqliteRemoteAuthorityStoreV1::from_connection(
            rusqlite::Connection::open(&repository_path)
                .map_err(|error| configuration_error(&error.to_string()))?,
        )
        .map_err(|error| configuration_error(&error.to_string()))?,
    );
    let port = CanonicalDaemonRemoteProtocolOwnersV1::new_with_registered_enrollment_and_query(
        enrollment_store,
        &enrollment_configuration_path,
        authority,
        Arc::clone(&repository),
    )
    .map_err(|error| configuration_error(&error.to_string()))?;
    let listener = match RemoteBrainHttpsService::bind_query_protocol(&https, port).await {
        Ok(listener) => listener,
        Err(error) => {
            let _ = repository.drain();
            let _ = repository.close_and_join();
            return Err(configuration_error(&error.to_string()));
        }
    };
    Ok(DaemonRemoteQueryRuntimeV1::Running {
        listener,
        repository,
    })
}

fn load_runtime_configuration(path: &Path) -> Result<Option<RemoteBrainRuntimeConfigV1>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(configuration_error(&error.to_string())),
    };
    let mut bytes = Vec::with_capacity(MAX_REMOTE_RUNTIME_CONFIG_BYTES as usize);
    file.take(MAX_REMOTE_RUNTIME_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| configuration_error(&error.to_string()))?;
    if bytes.len() as u64 > MAX_REMOTE_RUNTIME_CONFIG_BYTES {
        return Err(configuration_error(
            "Remote query runtime configuration exceeds its size limit",
        ));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| configuration_error("Remote query runtime configuration is invalid"))
}

fn configuration_error(message: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
    use tracedecay_store::{
        StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
    };

    use super::*;

    fn binding() -> StoreRuntimeBindingV1 {
        StoreRuntimeBindingV1::new(
            StoreShardIdV1::project_sessions(
                BrainId::new("brain.remote-query-runtime").unwrap(),
                UserProfileId::new("profile.remote-query-runtime").unwrap(),
                ProjectId::new("project.remote-query-runtime").unwrap(),
            ),
            StoreIncarnationV1::new(1).unwrap(),
            StoreAuthorityEpochV1::new(1).unwrap(),
        )
    }

    #[tokio::test]
    async fn missing_runtime_configuration_is_truthfully_unconfigured() {
        let root = tempfile::tempdir().unwrap();
        let runtime = start_daemon_remote_query_runtime(root.path()).await.unwrap();
        assert_eq!(runtime.state(), RemoteBrainHttpsStateV1::Unconfigured);
        assert_eq!(runtime.endpoint(), None);
    }

    #[tokio::test]
    async fn disabled_transport_does_not_construct_query_storage() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("https.json"),
            serde_json::json!({
                "version": 1,
                "enablement": "disabled"
            })
            .to_string(),
        )
        .unwrap();
        write_runtime_config(root.path(), "missing.sqlite");

        let runtime = start_daemon_remote_query_runtime(root.path()).await.unwrap();
        assert_eq!(runtime.state(), RemoteBrainHttpsStateV1::Disabled);
        assert_eq!(runtime.endpoint(), None);
    }

    #[tokio::test]
    async fn configured_startup_constructs_and_mounts_query_runtime() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("remote-query.sqlite");
        rusqlite::Connection::open(&database).unwrap();
        std::fs::write(
            root.path().join("enrollment.json"),
            serde_json::json!({ "grants": [] }).to_string(),
        )
        .unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/remote-tls");
        std::fs::write(
            root.path().join("https.json"),
            serde_json::json!({
                "version": 1,
                "enablement": "enabled",
                "bind_address": "127.0.0.1:0",
                "advertised_endpoint": "https://localhost",
                "certificate_chain_path": fixtures.join("server.pem"),
                "private_key_path": fixtures.join("server-key.pem"),
                "client_ca_bundle_path": fixtures.join("ca.pem")
            })
            .to_string(),
        )
        .unwrap();
        write_runtime_config(root.path(), "remote-query.sqlite");

        let runtime = start_daemon_remote_query_runtime(root.path()).await.unwrap();
        assert_eq!(runtime.state(), RemoteBrainHttpsStateV1::Degraded);
        assert!(runtime.endpoint().is_some());
        runtime.shutdown().await;
    }

    fn write_runtime_config(root: &Path, database: &str) {
        std::fs::write(
            root.join(REMOTE_RUNTIME_CONFIG_FILE),
            serde_json::json!({
                "version": 1,
                "https_configuration_path": "https.json",
                "enrollment_configuration_path": "enrollment.json",
                "repository_database_path": database,
                "repository_binding": binding()
            })
            .to_string(),
        )
        .unwrap();
    }
}
