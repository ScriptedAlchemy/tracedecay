use std::io::{BufReader, ErrorKind, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls;
use tower::ServiceExt;
use tracedecay_application::remote::protocol::{
    RemoteEnrollmentProtocolPortV1, RemoteProtocolPortV1,
};
use tracedecay_application::remote::query::{RemoteQueryRequestV1, RemoteQueryResultV1};
use tracedecay_application::remote::recovery::{
    BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
    StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use tracedecay_application::remote::replay::{RemoteReplayOutcomeV1, RemoteReplayRequestV1};

const REMOTE_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_CONNECTION_DRAIN_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REMOTE_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_REMOTE_CONNECTIONS: usize = 64;

/// Versioned daemon-owned Remote Brain HTTPS listener configuration.
///
/// The default is intentionally not "disabled": it records that the operator
/// has not configured a remote endpoint, so status and Doctor can distinguish
/// an absent deployment decision from an explicitly disabled listener.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteBrainHttpsConfigV1 {
    pub version: u32,
    #[serde(default)]
    pub enablement: RemoteBrainHttpsEnablementV1,
    #[serde(default)]
    pub bind_address: Option<String>,
    #[serde(default)]
    pub advertised_endpoint: Option<String>,
    #[serde(default)]
    pub certificate_chain_path: Option<PathBuf>,
    #[serde(default)]
    pub private_key_path: Option<PathBuf>,
    #[serde(default)]
    pub client_ca_bundle_path: Option<PathBuf>,
}

impl Default for RemoteBrainHttpsConfigV1 {
    fn default() -> Self {
        Self {
            version: 1,
            enablement: RemoteBrainHttpsEnablementV1::Unconfigured,
            bind_address: None,
            advertised_endpoint: None,
            certificate_chain_path: None,
            private_key_path: None,
            client_ca_bundle_path: None,
        }
    }
}

impl RemoteBrainHttpsConfigV1 {
    pub fn load_optional(path: Option<&Path>) -> Result<Self, RemoteBrainHttpsError> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let file = std::fs::File::open(path).map_err(RemoteBrainHttpsError::ConfigIo)?;
        let mut bytes = Vec::with_capacity(MAX_REMOTE_CONFIG_BYTES as usize);
        file.take(MAX_REMOTE_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(RemoteBrainHttpsError::ConfigIo)?;
        if bytes.len() as u64 > MAX_REMOTE_CONFIG_BYTES {
            return Err(RemoteBrainHttpsError::InvalidConfiguration);
        }
        let mut config: Self = serde_json::from_slice(&bytes)
            .map_err(|_| RemoteBrainHttpsError::InvalidConfiguration)?;
        config.resolve_relative_tls_paths(path.parent().unwrap_or_else(|| Path::new(".")));
        config.validate()?;
        Ok(config)
    }

    fn resolve_relative_tls_paths(&mut self, config_directory: &Path) {
        for path in [
            &mut self.certificate_chain_path,
            &mut self.private_key_path,
            &mut self.client_ca_bundle_path,
        ] {
            if let Some(path) = path
                && path.is_relative()
            {
                *path = config_directory.join(&*path);
            }
        }
    }

    pub fn validate(&self) -> Result<(), RemoteBrainHttpsError> {
        if self.version != 1 {
            return Err(RemoteBrainHttpsError::UnsupportedVersion(self.version));
        }
        match self.enablement {
            RemoteBrainHttpsEnablementV1::Enabled => {
                self.validate_enabled()?;
            }
            RemoteBrainHttpsEnablementV1::Unconfigured | RemoteBrainHttpsEnablementV1::Disabled => {
                if self.bind_address.is_some()
                    || self.advertised_endpoint.is_some()
                    || self.certificate_chain_path.is_some()
                    || self.private_key_path.is_some()
                    || self.client_ca_bundle_path.is_some()
                {
                    return Err(RemoteBrainHttpsError::InvalidConfiguration);
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn state(&self) -> RemoteBrainHttpsStateV1 {
        match self.enablement {
            RemoteBrainHttpsEnablementV1::Unconfigured => RemoteBrainHttpsStateV1::Unconfigured,
            RemoteBrainHttpsEnablementV1::Disabled => RemoteBrainHttpsStateV1::Disabled,
            RemoteBrainHttpsEnablementV1::Enabled => RemoteBrainHttpsStateV1::Degraded,
        }
    }

    pub fn validate_enabled(&self) -> Result<RemoteBrainHttpsBindingV1, RemoteBrainHttpsError> {
        if self.version != 1 {
            return Err(RemoteBrainHttpsError::UnsupportedVersion(self.version));
        }
        if self.enablement != RemoteBrainHttpsEnablementV1::Enabled {
            return Err(RemoteBrainHttpsError::NotEnabled(self.state()));
        }
        let bind_address = self
            .bind_address
            .as_deref()
            .ok_or(RemoteBrainHttpsError::MissingField("bind_address"))?
            .parse()
            .map_err(|_| RemoteBrainHttpsError::InvalidBindAddress)?;
        let advertised_endpoint = self
            .advertised_endpoint
            .as_deref()
            .ok_or(RemoteBrainHttpsError::MissingField("advertised_endpoint"))?;
        let endpoint = url::Url::parse(advertised_endpoint)
            .map_err(|_| RemoteBrainHttpsError::InvalidAdvertisedEndpoint)?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || endpoint.username() != ""
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(RemoteBrainHttpsError::InvalidAdvertisedEndpoint);
        }
        let certificate_chain_path =
            self.certificate_chain_path
                .clone()
                .ok_or(RemoteBrainHttpsError::MissingField(
                    "certificate_chain_path",
                ))?;
        let private_key_path = self
            .private_key_path
            .clone()
            .ok_or(RemoteBrainHttpsError::MissingField("private_key_path"))?;
        let client_ca_bundle_path = self
            .client_ca_bundle_path
            .clone()
            .ok_or(RemoteBrainHttpsError::MissingField("client_ca_bundle_path"))?;
        Ok(RemoteBrainHttpsBindingV1 {
            bind_address,
            advertised_endpoint: endpoint,
            certificate_chain_path,
            private_key_path,
            client_ca_bundle_path,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RemoteBrainHttpsEnablementV1 {
    #[default]
    Unconfigured,
    Disabled,
    Enabled,
}

/// Fail-closed lifecycle state exposed before a listener starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteBrainHttpsStateV1 {
    Unconfigured,
    Disabled,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBrainHttpsBindingV1 {
    pub bind_address: SocketAddr,
    pub advertised_endpoint: url::Url,
    pub certificate_chain_path: PathBuf,
    pub private_key_path: PathBuf,
    pub client_ca_bundle_path: PathBuf,
}

#[derive(Debug, Error)]
pub enum RemoteBrainHttpsError {
    #[error("unsupported Remote Brain HTTPS configuration version {0}")]
    UnsupportedVersion(u32),
    #[error("Remote Brain HTTPS listener is not enabled: {0:?}")]
    NotEnabled(RemoteBrainHttpsStateV1),
    #[error("Remote Brain HTTPS configuration is missing {0}")]
    MissingField(&'static str),
    #[error("Remote Brain HTTPS bind address is invalid")]
    InvalidBindAddress,
    #[error("Remote Brain advertised endpoint must be a credential-free HTTPS URL")]
    InvalidAdvertisedEndpoint,
    #[error("Remote Brain TLS credentials are unavailable or invalid")]
    TlsCredentials,
    #[error("Remote Brain HTTPS configuration is invalid")]
    InvalidConfiguration,
    #[error("Remote Brain HTTPS configuration could not be read: {0}")]
    ConfigIo(std::io::Error),
    #[error("Remote Brain HTTPS listener failed: {0}")]
    Io(#[from] std::io::Error),
}

/// A daemon-owned TLS-only listener. It intentionally accepts a pre-built
/// canonical router; local dashboard HTTP is never routed through this service.
pub struct RemoteBrainHttpsService {
    endpoint: SocketAddr,
    shutdown: watch::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), RemoteBrainHttpsError>>,
}

impl RemoteBrainHttpsService {
    pub async fn bind_query_protocol<Port>(
        config: &RemoteBrainHttpsConfigV1,
        port: Port,
    ) -> Result<Self, RemoteBrainHttpsError>
    where
        Port: RemoteEnrollmentProtocolPortV1
            + RemoteProtocolPortV1<RemoteQueryRequestV1, Output = RemoteQueryResultV1>
            + Send
            + Sync
            + 'static,
    {
        let router =
            tracedecay_api::remote::remote_query_protocol_router::<Port, RemoteQueryRequestV1>(
                port,
            );
        Self::bind(config, router).await
    }

    /// Mount exactly the canonical Remote Brain protocol router on this TLS
    /// listener. Local dashboard/application routes are intentionally absent.
    pub async fn bind_protocol<Port>(
        config: &RemoteBrainHttpsConfigV1,
        port: Port,
    ) -> Result<Self, RemoteBrainHttpsError>
    where
        Port: RemoteEnrollmentProtocolPortV1
            + RemoteProtocolPortV1<RemoteReplayRequestV1, Output = RemoteReplayOutcomeV1>
            + RemoteProtocolPortV1<RemoteQueryRequestV1, Output = RemoteQueryResultV1>
            + RemoteProtocolPortV1<BackupRequestV1, Output = BackupOperationStateV1>
            + RemoteProtocolPortV1<StagedRestoreConfirmationV1, Output = StagedRestoreProgressV1>
            + RemoteProtocolPortV1<PromotionConfirmationV1, Output = PromotionCasReceiptV1>
            + Send
            + Sync
            + 'static,
    {
        let router =
            tracedecay_api::remote::remote_protocol_router::<Port, RemoteQueryRequestV1>(port);
        Self::bind(config, router).await
    }

    async fn bind(
        config: &RemoteBrainHttpsConfigV1,
        router: Router,
    ) -> Result<Self, RemoteBrainHttpsError> {
        let binding = config.validate_enabled()?;
        let tls = TlsAcceptor::from(Arc::new(load_server_config(&binding)?));
        let listener = TcpListener::bind(binding.bind_address).await?;
        let endpoint = listener.local_addr()?;
        let (shutdown, shutdown_receiver) = watch::channel(());
        let task = tokio::spawn(run_listener(listener, tls, router, shutdown_receiver));
        Ok(Self {
            endpoint,
            shutdown,
            task,
        })
    }

    #[must_use]
    pub const fn endpoint(&self) -> SocketAddr {
        self.endpoint
    }

    pub async fn shutdown(self) -> Result<(), RemoteBrainHttpsError> {
        let _ = self.shutdown.send(());
        self.task
            .await
            .map_err(|error| RemoteBrainHttpsError::Io(std::io::Error::other(error)))?
    }

    #[cfg(test)]
    pub(super) async fn bind_test_router(
        config: &RemoteBrainHttpsConfigV1,
        router: Router,
    ) -> Result<Self, RemoteBrainHttpsError> {
        Self::bind(config, router).await
    }
}

async fn run_listener(
    listener: TcpListener,
    tls: TlsAcceptor,
    router: Router,
    mut shutdown: watch::Receiver<()>,
) -> Result<(), RemoteBrainHttpsError> {
    let admission = Arc::new(Semaphore::new(MAX_REMOTE_CONNECTIONS));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = joined {
                    return Err(RemoteBrainHttpsError::Io(std::io::Error::other(error)));
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let Ok(permit) = Arc::clone(&admission).try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let tls = tls.clone();
                let router = router.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let Ok(stream) = timeout(REMOTE_TLS_HANDSHAKE_TIMEOUT, tls.accept(stream)).await else {
                        return;
                    };
                    let Ok(stream) = stream else {
                        return;
                    };
                    let service = service_fn(move |request: hyper::Request<Incoming>| {
                        let router = router.clone();
                        async move {
                            Ok::<_, std::convert::Infallible>(
                                router
                                    .oneshot(request.map(axum::body::Body::new))
                                    .await
                                    .expect("axum router is infallible"),
                            )
                        }
                    });
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        }
    }
    drop(listener);
    match timeout(REMOTE_CONNECTION_DRAIN_TIMEOUT, async {
        while let Some(joined) = connections.join_next().await {
            joined.map_err(|error| RemoteBrainHttpsError::Io(std::io::Error::other(error)))?;
        }
        Ok(())
    })
    .await
    {
        Ok(result) => result,
        Err(_) => {
            connections.abort_all();
            while connections.join_next().await.is_some() {}
            Err(RemoteBrainHttpsError::Io(std::io::Error::new(
                ErrorKind::TimedOut,
                "Remote Brain HTTPS listener did not drain",
            )))
        }
    }
}

fn load_server_config(
    binding: &RemoteBrainHttpsBindingV1,
) -> Result<rustls::ServerConfig, RemoteBrainHttpsError> {
    let certificate_file = std::fs::File::open(&binding.certificate_chain_path)
        .map_err(|_| RemoteBrainHttpsError::TlsCredentials)?;
    let certificates = rustls_pemfile::certs(&mut BufReader::new(certificate_file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| RemoteBrainHttpsError::TlsCredentials)?;
    if certificates.is_empty() {
        return Err(RemoteBrainHttpsError::TlsCredentials);
    }
    let private_key_file = std::fs::File::open(&binding.private_key_path)
        .map_err(|_| RemoteBrainHttpsError::TlsCredentials)?;
    let private_key = rustls_pemfile::private_key(&mut BufReader::new(private_key_file))
        .map_err(|_| RemoteBrainHttpsError::TlsCredentials)?
        .ok_or(RemoteBrainHttpsError::TlsCredentials)?;
    let client_ca_file = std::fs::File::open(&binding.client_ca_bundle_path)
        .map_err(|_| RemoteBrainHttpsError::TlsCredentials)?;
    let client_ca_certificates = rustls_pemfile::certs(&mut BufReader::new(client_ca_file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| RemoteBrainHttpsError::TlsCredentials)?;
    if client_ca_certificates.is_empty() {
        return Err(RemoteBrainHttpsError::TlsCredentials);
    }
    let mut client_roots = rustls::RootCertStore::empty();
    for certificate in client_ca_certificates {
        client_roots
            .add(certificate)
            .map_err(|_| RemoteBrainHttpsError::TlsCredentials)?;
    }
    let client_verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(client_roots))
        .build()
        .map_err(|_| RemoteBrainHttpsError::TlsCredentials)?;
    rustls::ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(certificates, private_key)
        .map_err(|_| RemoteBrainHttpsError::TlsCredentials)
}
