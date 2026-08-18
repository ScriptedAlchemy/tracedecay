//! Daemon-owned loopback HTTP lifecycle for canonical application routers.
//!
//! This is intentionally independent of the optional dashboard server. The
//! outer service owns only local transport admission and project routing;
//! every mounted inner router remains the canonical application adapter.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::RwLock as SyncRwLock;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Json, Path, Request, State};
use axum::http::header::{AUTHORIZATION, CONNECTION, ORIGIN};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use constant_time_eq::constant_time_eq;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use rustls::client::{verify_server_cert_signed_by_trust_anchor, verify_server_name};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use tracedecay_application::remote::auth::RemoteEnrollmentAdmissionEvidenceV1;
use tracedecay_application::remote::status::RemoteOperationalStatusReadV1;
use tracedecay_application::{
    APPLICATION_REQUEST_ID_HEADER, ApplicationProblem, LegalAction, RequestId, RetryDirective,
    SafeDiagnostic,
};
use tracedecay_domain::{EnrollmentGrantV1, ProjectId};

use crate::errors::{Result, TraceDecayError};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

const MAX_HTTP_APPLICATION_PROJECT_ROUTERS: usize = 8;
const MAX_HTTP_APPLICATION_COLD_RESOLUTIONS: usize = 8;
const HTTP_APPLICATION_COLD_RESOLUTION_DEADLINE: Duration = Duration::from_secs(5);
const MAX_REMOTE_BRAIN_TLS_CONNECTIONS: usize = 128;
const MAX_REMOTE_BRAIN_TLS_HEADER_BYTES: usize = 64 * 1024;
const REMOTE_BRAIN_TLS_READ_IDLE_DEADLINE: Duration = Duration::from_secs(5);
const REMOTE_BRAIN_TLS_REQUEST_READ_DEADLINE: Duration = Duration::from_mins(1);
const REMOTE_BRAIN_TLS_WRITE_IDLE_DEADLINE: Duration = Duration::from_secs(5);
const REMOTE_BRAIN_TLS_RESPONSE_DEADLINE: Duration = Duration::from_secs(30);
const REMOTE_BRAIN_TLS_CLOSE_DEADLINE: Duration = Duration::from_secs(5);
const REMOTE_BRAIN_TLS_SHUTDOWN_DRAIN_DEADLINE: Duration = Duration::from_secs(5);

type ProjectRouterResolverFuture =
    Pin<Box<dyn Future<Output = Result<Option<Router>>> + Send + 'static>>;
type ProjectRouterResolver =
    Arc<dyn Fn(ProjectId) -> ProjectRouterResolverFuture + Send + Sync + 'static>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectRouterResolutionError {
    Saturated,
    TimedOut,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectRouterProblem {
    NotFoundOrNotAuthorized,
    Saturated,
    TimedOut,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OuterApplicationRequestIdError {
    DuplicateHeader,
    InvalidHeader,
    DisallowedOperation,
}

#[derive(Default)]
struct ProjectRouterCache {
    routers: HashMap<String, Router>,
    least_recently_used: VecDeque<String>,
}

impl ProjectRouterCache {
    fn get(&mut self, project_id: &str) -> Option<Router> {
        let router = self.routers.get(project_id).cloned()?;
        self.touch(project_id);
        Some(router)
    }

    fn insert(&mut self, project_id: String, router: Router) {
        if !self.routers.contains_key(&project_id)
            && self.routers.len() >= MAX_HTTP_APPLICATION_PROJECT_ROUTERS
            && let Some(evicted) = self.least_recently_used.pop_front()
        {
            self.routers.remove(&evicted);
        }
        self.routers.insert(project_id.clone(), router);
        self.touch(&project_id);
    }

    fn touch(&mut self, project_id: &str) {
        self.least_recently_used
            .retain(|candidate| candidate != project_id);
        self.least_recently_used.push_back(project_id.to_owned());
    }

    fn remove(&mut self, project_id: &str) {
        self.routers.remove(project_id);
        self.least_recently_used
            .retain(|candidate| candidate != project_id);
    }

    fn clear(&mut self) {
        self.routers.clear();
        self.least_recently_used.clear();
    }
}

#[derive(Clone)]
struct RemoteHttpApplicationMount {
    router: Router,
    credentials: Arc<super::remote_protocol::DaemonRemoteCredentialAuthorityV1>,
    runtime: Option<Arc<super::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteNodeProvisionRequestV1 {
    grant: EnrollmentGrantV1,
    admission: RemoteEnrollmentAdmissionEvidenceV1,
}

#[derive(Clone)]
pub(super) struct DaemonHttpApplicationRegistry {
    routers: Arc<Mutex<ProjectRouterCache>>,
    resolver: Arc<SyncRwLock<Option<ProjectRouterResolver>>>,
    resolver_admission: Arc<Semaphore>,
    remote: Arc<SyncRwLock<Option<RemoteHttpApplicationMount>>>,
    active: Arc<AtomicBool>,
    remote_deletion_runtime_owners:
        Arc<SyncRwLock<Option<super::remote_deletion::RemoteDeletionRuntimeOwners>>>,
}

impl Default for DaemonHttpApplicationRegistry {
    fn default() -> Self {
        Self {
            routers: Arc::new(Mutex::new(ProjectRouterCache::default())),
            resolver: Arc::new(SyncRwLock::new(None)),
            resolver_admission: Arc::new(Semaphore::new(MAX_HTTP_APPLICATION_COLD_RESOLUTIONS)),
            remote: Arc::new(SyncRwLock::new(None)),
            active: Arc::new(AtomicBool::new(false)),
            remote_deletion_runtime_owners: Arc::new(SyncRwLock::new(None)),
        }
    }
}

impl DaemonHttpApplicationRegistry {
    pub(super) async fn mount(&self, project_id: &str, router: Router) -> Result<()> {
        let project_id =
            ProjectId::new(project_id.to_owned()).map_err(|error| TraceDecayError::Config {
                message: format!("daemon HTTP project identity is invalid: {error}"),
            })?;
        self.routers
            .lock()
            .await
            .insert(project_id.as_str().to_owned(), router);
        Ok(())
    }

    pub(super) fn install_resolver<F, Fut>(&self, resolver: F) -> Result<()>
    where
        F: Fn(ProjectId) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Router>>> + Send + 'static,
    {
        let mut slot = self.resolver.write().map_err(|_| TraceDecayError::Config {
            message: "daemon HTTP project resolver lock is poisoned".to_owned(),
        })?;
        if slot.is_some() {
            return Err(TraceDecayError::Config {
                message: "daemon HTTP project resolver is already installed".to_owned(),
            });
        }
        *slot = Some(Arc::new(move |project_id| Box::pin(resolver(project_id))));
        Ok(())
    }

    pub(super) fn install_remote(
        &self,
        router: Router,
        credentials: Arc<super::remote_protocol::DaemonRemoteCredentialAuthorityV1>,
        runtime: Option<
            Arc<super::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>,
        >,
    ) -> Result<()> {
        let mut slot = self.remote.write().map_err(|_| TraceDecayError::Config {
            message: "daemon HTTP Remote Brain router lock is poisoned".to_owned(),
        })?;
        if slot.is_some() {
            return Err(TraceDecayError::Config {
                message: "daemon HTTP Remote Brain router is already installed".to_owned(),
            });
        }
        *slot = Some(RemoteHttpApplicationMount {
            router,
            credentials,
            runtime,
        });
        Ok(())
    }

    pub(super) fn install_remote_deletion_runtime_owners(
        &self,
        owners: super::remote_deletion::RemoteDeletionRuntimeOwners,
    ) -> Result<()> {
        let mut slot =
            self.remote_deletion_runtime_owners
                .write()
                .map_err(|_| TraceDecayError::Config {
                    message: "daemon remote deletion executor lock is poisoned".to_owned(),
                })?;
        if slot.is_some() {
            return Err(TraceDecayError::Config {
                message: "daemon remote deletion executor is already installed".to_owned(),
            });
        }
        *slot = Some(owners);
        Ok(())
    }

    pub(super) fn remote_deletion_runtime_owners(
        &self,
    ) -> Result<Option<super::remote_deletion::RemoteDeletionRuntimeOwners>> {
        self.remote_deletion_runtime_owners
            .read()
            .map(|executor| executor.clone())
            .map_err(|_| TraceDecayError::Config {
                message: "daemon remote deletion executor lock is poisoned".to_owned(),
            })
    }

    pub(super) async fn forget_remote_deleted_routes(
        &self,
        target: super::remote_deletion::RemoteDeletionReceiptTarget,
        project_id: Option<&str>,
    ) {
        let mut routers = self.routers.lock().await;
        match target {
            super::remote_deletion::RemoteDeletionReceiptTarget::Account => routers.clear(),
            super::remote_deletion::RemoteDeletionReceiptTarget::Project => {
                if let Some(project_id) = project_id {
                    routers.remove(project_id);
                }
            }
        }
    }

    async fn resolve(
        &self,
        project_id: &str,
    ) -> std::result::Result<Option<Router>, ProjectRouterResolutionError> {
        let Ok(project_id) = ProjectId::new(project_id.to_owned()) else {
            return Ok(None);
        };
        if let Some(router) = self.routers.lock().await.get(project_id.as_str()) {
            return Ok(Some(router));
        }
        let resolver = {
            let slot = self
                .resolver
                .read()
                .map_err(|_| ProjectRouterResolutionError::Unavailable)?;
            slot.as_ref().cloned()
        };
        let Some(resolver) = resolver else {
            return Ok(None);
        };
        let _permit = Arc::clone(&self.resolver_admission)
            .try_acquire_owned()
            .map_err(|_| ProjectRouterResolutionError::Saturated)?;
        // A cold project route resolution can park on daemon project-open
        // work; bound it so an HTTP caller never waits unboundedly.
        let resolved = tokio::time::timeout(
            HTTP_APPLICATION_COLD_RESOLUTION_DEADLINE,
            resolver(project_id.clone()),
        )
        .await
        .map_err(|_| ProjectRouterResolutionError::TimedOut)?
        .map_err(|_| ProjectRouterResolutionError::Unavailable)?;
        let Some(router) = resolved else {
            return Ok(None);
        };
        self.routers
            .lock()
            .await
            .insert(project_id.as_str().to_owned(), router.clone());
        Ok(Some(router))
    }

    fn router(
        self,
        admission: LocalHttpAdmission,
    ) -> Result<(
        Router,
        Option<Arc<super::remote_protocol::DaemonRemoteCredentialAuthorityV1>>,
    )> {
        let local = Router::new()
            .route(
                "/projects/{project_id}/application/{*tail}",
                any(dispatch_project_application),
            )
            .route("/remote-nodes/provision", post(provision_remote_node))
            .route("/remote-status", get(remote_operational_status))
            // Deletion lifecycle intake. The upstream lanes mounted this at
            // `/remote/deletions`; at this tip `/remote` is a nest point for the
            // Remote Brain router, so the local admission surface uses the same
            // hyphenated convention as `/remote-nodes/provision` to stay
            // conflict-free with that nest.
            .route(
                "/remote-deletions",
                post(super::remote_deletion::dispatch_remote_deletion),
            )
            .with_state(self.clone())
            .layer(middleware::from_fn_with_state(
                admission,
                require_local_http_admission,
            ));
        let remote = self
            .remote
            .read()
            .map_err(|_| TraceDecayError::Config {
                message: "daemon HTTP Remote Brain router lock is poisoned".to_owned(),
            })?
            .clone();
        match remote {
            Some(remote) => Ok((
                local.nest("/remote", remote.router),
                Some(remote.credentials),
            )),
            None => Ok((local, None)),
        }
    }

    fn remote_protocol_router(
        &self,
    ) -> Result<
        Option<(
            Router,
            Arc<super::remote_protocol::DaemonRemoteCredentialAuthorityV1>,
        )>,
    > {
        let remote = self
            .remote
            .read()
            .map_err(|_| TraceDecayError::Config {
                message: "daemon HTTP Remote Brain router lock is poisoned".to_owned(),
            })?
            .clone();
        Ok(remote.map(|remote| (remote.router, remote.credentials)))
    }

    pub(super) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
}

async fn provision_remote_node(
    State(registry): State<DaemonHttpApplicationRegistry>,
    Json(request): Json<RemoteNodeProvisionRequestV1>,
) -> Response {
    let runtime = registry.remote.read().ok().and_then(|remote| {
        remote
            .as_ref()
            .and_then(|remote| remote.runtime.as_ref().map(Arc::clone))
    });
    let Some(runtime) = runtime else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match runtime
        .provision_remote_node(request.grant, request.admission)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::CONFLICT.into_response(),
    }
}

async fn remote_operational_status(
    State(registry): State<DaemonHttpApplicationRegistry>,
) -> Response {
    let status = match registry.remote.read() {
        Ok(remote) => remote
            .as_ref()
            .and_then(|remote| remote.runtime.as_ref())
            .map(|runtime| runtime.remote_operational_status())
            .unwrap_or(RemoteOperationalStatusReadV1::Unavailable),
        Err(_) => RemoteOperationalStatusReadV1::Unavailable,
    };
    Json(status).into_response()
}

const REMOTE_STATUS_HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// Reads the live daemon's mounted Remote Brain operational state over the
/// authenticated loopback HTTP application. Never opens a local store or
/// constructs a fresh in-process registry.
pub(crate) fn live_remote_operational_status() -> Result<RemoteOperationalStatusReadV1> {
    let connection = super::current_daemon_connection()?;
    let Some(record) = connection.authority_record.as_ref() else {
        return Err(missing_daemon_authority());
    };
    let Some(endpoint) = record.http_application_endpoint else {
        return Err(TraceDecayError::Config {
            message: "TraceDecay daemon HTTP application endpoint is not published. Start or restart the daemon.".to_owned(),
        });
    };
    let Some(auth_token) = connection.auth_token.as_deref() else {
        return Err(missing_daemon_authority());
    };
    let origin = format!("http://{endpoint}");
    let url = format!("http://{endpoint}/remote-status");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(REMOTE_STATUS_HTTP_TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(&url)
        .header("Authorization", format!("Bearer {auth_token}"))
        .header("Origin", origin)
        .call()
        .map_err(|_| remote_status_daemon_unavailable())?;
    if response.status().as_u16() != 200 {
        return Err(TraceDecayError::Config {
            message: format!(
                "TraceDecay daemon remote status returned HTTP {}. Start or restart the daemon.",
                response.status()
            ),
        });
    }
    response
        .body_mut()
        .read_json()
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "TraceDecay daemon remote status was not a typed operational read: {error}"
            ),
        })
}

fn missing_daemon_authority() -> TraceDecayError {
    TraceDecayError::Config {
        message:
            "TraceDecay daemon authority record is not available. Start or restart the daemon."
                .to_owned(),
    }
}

fn remote_status_daemon_unavailable() -> TraceDecayError {
    match super::default_socket_path() {
        Ok(socket_path) => super::unavailable_error(&socket_path),
        Err(error) => error,
    }
}

async fn dispatch_project_application(
    State(registry): State<DaemonHttpApplicationRegistry>,
    Path((project_id, tail)): Path<(String, String)>,
    mut request: Request<Body>,
) -> Response {
    let request_id = match outer_application_request_id(&tail, request.headers()) {
        Ok(request_id) => request_id,
        Err(_) => return invalid_request_control_response(),
    };
    let router = match registry.resolve(&project_id).await {
        Ok(Some(router)) => router,
        Ok(None) => {
            return project_router_problem_response(
                request_id,
                ProjectRouterProblem::NotFoundOrNotAuthorized,
            );
        }
        Err(ProjectRouterResolutionError::Saturated) => {
            return project_router_problem_response(request_id, ProjectRouterProblem::Saturated);
        }
        Err(ProjectRouterResolutionError::TimedOut) => {
            return project_router_problem_response(request_id, ProjectRouterProblem::TimedOut);
        }
        Err(ProjectRouterResolutionError::Unavailable) => {
            return project_router_problem_response(request_id, ProjectRouterProblem::Unavailable);
        }
    };
    let query = request
        .uri()
        .query()
        .map_or_else(String::new, |query| format!("?{query}"));
    let Ok(uri) = format!("/{tail}{query}").parse::<Uri>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    *request.uri_mut() = uri;
    request.extensions_mut().clear();
    match router.oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

fn outer_application_request_id(
    tail: &str,
    headers: &HeaderMap,
) -> std::result::Result<Option<RequestId>, OuterApplicationRequestIdError> {
    let mut values = headers.get_all(APPLICATION_REQUEST_ID_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(OuterApplicationRequestIdError::DuplicateHeader);
    }
    let curate_path = tracedecay_api::retained_route_path(
        tracedecay_application::retained_surfaces::RetainedSurfaceOperation::FactStoreCurate,
    );
    if curate_path.strip_prefix('/') != Some(tail) {
        return Err(OuterApplicationRequestIdError::DisallowedOperation);
    }
    let value = value
        .to_str()
        .map_err(|_| OuterApplicationRequestIdError::InvalidHeader)?;
    RequestId::new(value.to_owned())
        .map(Some)
        .map_err(|_| OuterApplicationRequestIdError::InvalidHeader)
}

fn invalid_request_control_response() -> Response {
    let Ok(request_id) = mint_global_request_id(GlobalRequestSurface::Http) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    tracedecay_api::retained_invalid_request_response(request_id)
}

fn project_router_problem_response(
    request_id: Option<RequestId>,
    problem: ProjectRouterProblem,
) -> Response {
    let problem = match problem {
        ProjectRouterProblem::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        ProjectRouterProblem::Saturated => {
            let Ok(diagnostic) = SafeDiagnostic::new(
                "http.project_router_saturated",
                "Project route resolution is saturated",
            ) else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            ApplicationProblem::Saturated {
                diagnostic,
                retry: RetryDirective::AfterDelay,
                legal_actions: vec![LegalAction::Retry],
            }
        }
        ProjectRouterProblem::TimedOut => ApplicationProblem::timed_out_before_admission(),
        ProjectRouterProblem::Unavailable => {
            let Ok(diagnostic) = SafeDiagnostic::new(
                "http.project_router_unavailable",
                "Project route resolution is unavailable",
            ) else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            ApplicationProblem::unavailable(diagnostic)
        }
    };
    transport_problem_response(request_id, problem)
}

fn transport_problem_response(
    request_id: Option<RequestId>,
    problem: ApplicationProblem,
) -> Response {
    let request_id = match request_id {
        Some(request_id) => request_id,
        None => {
            let Ok(request_id) = mint_global_request_id(GlobalRequestSurface::Http) else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            request_id
        }
    };
    tracedecay_api::adapter_problem_response(request_id, problem)
}

#[derive(Clone)]
struct LocalHttpAdmission {
    authorization: HeaderValue,
    origin: HeaderValue,
}

impl LocalHttpAdmission {
    fn new(auth_token: &str, endpoint: SocketAddr) -> Result<Self> {
        if auth_token.len() != 64 || !auth_token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(TraceDecayError::Config {
                message: "daemon HTTP authentication token is invalid".to_owned(),
            });
        }
        let authorization =
            HeaderValue::from_str(&format!("Bearer {auth_token}")).map_err(|_| {
                TraceDecayError::Config {
                    message: "daemon HTTP authentication token is not header-safe".to_owned(),
                }
            })?;
        let origin = HeaderValue::from_str(&format!("http://{endpoint}")).map_err(|_| {
            TraceDecayError::Config {
                message: "daemon HTTP loopback origin is not header-safe".to_owned(),
            }
        })?;
        Ok(Self {
            authorization,
            origin,
        })
    }
}

async fn require_local_http_admission(
    State(admission): State<LocalHttpAdmission>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let authorization_matches = request.headers().get(AUTHORIZATION).is_some_and(|actual| {
        let actual = actual.as_bytes();
        let expected = admission.authorization.as_bytes();
        actual.len() == expected.len() && constant_time_eq(actual, expected)
    });
    if !authorization_matches {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let origin_matches = request.headers().get(ORIGIN).is_some_and(|actual| {
        let actual = actual.as_bytes();
        let expected = admission.origin.as_bytes();
        actual.len() == expected.len() && constant_time_eq(actual, expected)
    });
    if !origin_matches {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

async fn force_remote_connection_close(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("close"));
    response
}

pub(super) struct DaemonHttpApplicationService {
    endpoint: SocketAddr,
    remote_tls_endpoint: Option<SocketAddr>,
    #[cfg(test)]
    origin: String,
    active: Arc<AtomicBool>,
    remote_credentials: Option<Arc<super::remote_protocol::DaemonRemoteCredentialAuthorityV1>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<()>>>,
    remote_tls_shutdown: Option<oneshot::Sender<()>>,
    remote_tls_task: Option<JoinHandle<Result<()>>>,
    #[cfg(test)]
    remote_tls_admission: Option<Arc<Semaphore>>,
    #[cfg(test)]
    remote_tls_egress: Option<Arc<RemoteBrainTlsEgressObserver>>,
}

struct RemoteBrainTlsServer {
    listener: RemoteBrainTlsListener,
    endpoint: SocketAddr,
    router: Router,
    admission: Arc<Semaphore>,
    credentials: Arc<super::remote_protocol::DaemonRemoteCredentialAuthorityV1>,
    #[cfg(test)]
    egress: Arc<RemoteBrainTlsEgressObserver>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RemoteBrainTlsEgressSnapshot {
    pub(super) active: usize,
    pub(super) backpressured: usize,
    pub(super) idle_expirations: usize,
    pub(super) idle_deadline_contract_violations: usize,
    pub(super) response_expirations: usize,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RemoteBrainTlsIngressSnapshot {
    pub(super) headers_complete: usize,
    pub(super) body_bytes_observed: usize,
}

#[cfg(test)]
#[derive(Default)]
struct RemoteBrainTlsEgressObserver {
    active: AtomicUsize,
    backpressured: AtomicUsize,
    idle_expirations: AtomicUsize,
    idle_deadline_contract_violations: AtomicUsize,
    response_expirations: AtomicUsize,
    headers_complete: AtomicUsize,
    body_bytes_observed: AtomicUsize,
}

#[cfg(test)]
impl RemoteBrainTlsEgressObserver {
    fn snapshot(&self) -> RemoteBrainTlsEgressSnapshot {
        RemoteBrainTlsEgressSnapshot {
            active: self.active.load(Ordering::SeqCst),
            backpressured: self.backpressured.load(Ordering::SeqCst),
            idle_expirations: self.idle_expirations.load(Ordering::SeqCst),
            idle_deadline_contract_violations: self
                .idle_deadline_contract_violations
                .load(Ordering::SeqCst),
            response_expirations: self.response_expirations.load(Ordering::SeqCst),
        }
    }

    fn ingress_snapshot(&self) -> RemoteBrainTlsIngressSnapshot {
        RemoteBrainTlsIngressSnapshot {
            headers_complete: self.headers_complete.load(Ordering::SeqCst),
            body_bytes_observed: self.body_bytes_observed.load(Ordering::SeqCst),
        }
    }
}

impl DaemonHttpApplicationService {
    #[cfg(test)]
    pub(super) async fn bind(
        registry: DaemonHttpApplicationRegistry,
        auth_token: &str,
    ) -> Result<Self> {
        Self::bind_with_remote_tls(registry, auth_token, None).await
    }

    pub(super) async fn bind_with_remote_tls(
        registry: DaemonHttpApplicationRegistry,
        auth_token: &str,
        remote_tls: Option<&super::bootstrap::RemoteBrainTlsConfig>,
    ) -> Result<Self> {
        let remote_tls_server = match remote_tls {
            Some(config) => {
                let Some((router, credentials)) = registry.remote_protocol_router()? else {
                    return Err(TraceDecayError::Config {
                        message: "Remote Brain TLS listener requires the canonical remote protocol router".to_owned(),
                    });
                };
                let listener = RemoteBrainTlsListener::bind(config).await?;
                credentials.publish_listener_serving();
                let endpoint = listener.bound_addr()?;
                let admission = Arc::clone(&listener.admission);
                #[cfg(test)]
                let egress = Arc::clone(&listener.egress);
                Some(RemoteBrainTlsServer {
                    listener,
                    endpoint,
                    router: Router::new()
                        .nest("/remote", router)
                        .layer(middleware::from_fn(force_remote_connection_close)),
                    admission,
                    credentials,
                    #[cfg(test)]
                    egress,
                })
            }
            None => None,
        };
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to bind daemon HTTP loopback listener: {error}"),
            })?;
        let endpoint = listener
            .local_addr()
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to read daemon HTTP loopback address: {error}"),
            })?;
        let admission = LocalHttpAdmission::new(auth_token, endpoint)?;
        #[cfg(test)]
        let origin = admission
            .origin
            .to_str()
            .map_err(|_| TraceDecayError::Config {
                message: "daemon HTTP loopback origin is not text".to_owned(),
            })?;
        let active = Arc::clone(&registry.active);
        let (app, remote_credentials) = registry.router(admission.clone())?;
        active.store(true, Ordering::Release);
        let (shutdown, shutdown_requested) = oneshot::channel();
        let task_active = Arc::clone(&active);
        let task = tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_requested.await;
                })
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("daemon HTTP application service failed: {error}"),
                });
            task_active.store(false, Ordering::Release);
            result
        });
        let mut remote_tls_endpoint = None;
        let mut remote_tls_shutdown = None;
        let mut remote_tls_task = None;
        let mut remote_tls_admission = None;
        #[cfg(test)]
        let mut remote_tls_egress = None;
        if let Some(server) = remote_tls_server {
            let (shutdown, shutdown_requested) = oneshot::channel();
            remote_tls_endpoint = Some(server.endpoint);
            remote_tls_shutdown = Some(shutdown);
            remote_tls_admission = Some(server.admission);
            #[cfg(test)]
            {
                remote_tls_egress = Some(server.egress);
            }
            let listener_state = server.credentials;
            remote_tls_task = Some(tokio::spawn(async move {
                let result =
                    serve_remote_brain_tls(server.listener, server.router, shutdown_requested)
                        .await;
                match &result {
                    Ok(()) => listener_state.publish_listener_stopped(),
                    Err(_) => listener_state.publish_listener_degraded(),
                }
                result
            }));
        }
        #[cfg(not(test))]
        drop(remote_tls_admission);
        Ok(Self {
            endpoint,
            remote_tls_endpoint,
            #[cfg(test)]
            origin: origin.to_owned(),
            active,
            remote_credentials,
            shutdown: Some(shutdown),
            task: Some(task),
            remote_tls_shutdown,
            remote_tls_task,
            #[cfg(test)]
            remote_tls_admission,
            #[cfg(test)]
            remote_tls_egress,
        })
    }

    pub(super) fn endpoint(&self) -> SocketAddr {
        self.endpoint
    }

    pub(super) fn remote_tls_endpoint(&self) -> Option<SocketAddr> {
        self.remote_tls_endpoint
    }

    #[cfg(test)]
    pub(super) fn remote_tls_available_admissions(&self) -> Option<usize> {
        self.remote_tls_admission
            .as_ref()
            .map(|admission| admission.available_permits())
    }

    #[cfg(test)]
    pub(super) fn remote_tls_admission(&self) -> Option<Arc<Semaphore>> {
        self.remote_tls_admission.as_ref().map(Arc::clone)
    }

    #[cfg(test)]
    pub(super) fn remote_tls_egress_snapshot(&self) -> Option<RemoteBrainTlsEgressSnapshot> {
        self.remote_tls_egress
            .as_ref()
            .map(|observer| observer.snapshot())
    }

    #[cfg(test)]
    pub(super) fn remote_tls_ingress_snapshot(&self) -> Option<RemoteBrainTlsIngressSnapshot> {
        self.remote_tls_egress
            .as_ref()
            .map(|observer| observer.ingress_snapshot())
    }

    #[cfg(test)]
    pub(super) fn origin(&self) -> &str {
        &self.origin
    }

    pub(super) async fn shutdown(mut self) -> Result<()> {
        self.active.store(false, Ordering::Release);
        if let Some(credentials) = self.remote_credentials.take() {
            credentials.cancel();
        }
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(shutdown) = self.remote_tls_shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        task.await.map_err(|error| TraceDecayError::Config {
            message: format!("daemon HTTP application service task failed: {error}"),
        })??;
        let Some(task) = self.remote_tls_task.take() else {
            return Ok(());
        };
        task.await.map_err(|error| TraceDecayError::Config {
            message: format!("Remote Brain TLS application service task failed: {error}"),
        })?
    }
}

impl Drop for DaemonHttpApplicationService {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        if let Some(credentials) = self.remote_credentials.take() {
            credentials.cancel();
        }
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(shutdown) = self.remote_tls_shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(task) = self.remote_tls_task.take() {
            task.abort();
        }
    }
}

struct RemoteBrainTlsListener {
    listener: tokio::net::TcpListener,
    server: Arc<rustls::ServerConfig>,
    admission: Arc<Semaphore>,
    #[cfg(test)]
    egress: Arc<RemoteBrainTlsEgressObserver>,
}

impl RemoteBrainTlsListener {
    async fn bind(config: &super::bootstrap::RemoteBrainTlsConfig) -> Result<Self> {
        let certificates = CertificateDer::pem_file_iter(config.certificate_chain())
            .map_err(|error| tls_configuration_error("open Remote Brain TLS certificate", error))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                tls_configuration_error("decode Remote Brain TLS certificate", error)
            })?;
        if certificates.len() < 2 {
            return Err(TraceDecayError::Config {
                message: "Remote Brain TLS certificate chain requires a leaf followed by an explicit trust anchor".to_owned(),
            });
        }
        let crypto_provider = Arc::new(rustls::crypto::ring::default_provider());
        validate_remote_brain_tls_identity(
            &certificates,
            config.listen(),
            UnixTime::now(),
            &crypto_provider,
        )?;
        let private_key_file = tracedecay_private_fs::open_private_file(config.private_key())
            .map_err(|error| {
                tls_configuration_error("open and validate Remote Brain TLS private key", error)
            })?;
        let private_key = PrivateKeyDer::from_pem_reader(private_key_file).map_err(|error| {
            tls_configuration_error("decode Remote Brain TLS private key", error)
        })?;
        let mut server = rustls::ServerConfig::builder_with_provider(crypto_provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| {
                tls_configuration_error("select Remote Brain TLS protocol versions", error)
            })?
            .with_no_client_auth()
            .with_single_cert(certificates, private_key)
            .map_err(|error| tls_configuration_error("bind Remote Brain TLS identity", error))?;
        server.alpn_protocols = vec![b"http/1.1".to_vec()];
        let listener = tokio::net::TcpListener::bind(config.listen())
            .await
            .map_err(|error| tls_configuration_error("bind Remote Brain TLS listener", error))?;
        Ok(Self {
            listener,
            server: Arc::new(server),
            admission: Arc::new(Semaphore::new(MAX_REMOTE_BRAIN_TLS_CONNECTIONS)),
            #[cfg(test)]
            egress: Arc::new(RemoteBrainTlsEgressObserver::default()),
        })
    }

    fn bound_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    async fn accept(&self) -> Option<(RemoteBrainTlsIo, SocketAddr)> {
        let (stream, address) = match self.listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "Remote Brain TLS listener accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
                return None;
            }
        };
        let permit = match Arc::clone(&self.admission).try_acquire_owned() {
            Ok(permit) => permit,
            Err(error) => {
                tracing::warn!(%error, %address, "Remote Brain TLS connection admission saturated");
                return None;
            }
        };
        let handshake = tokio_rustls::TlsAcceptor::from(Arc::clone(&self.server)).accept(stream);
        Some((
            RemoteBrainTlsIo::new(
                handshake,
                permit,
                #[cfg(test)]
                Arc::clone(&self.egress),
            ),
            address,
        ))
    }
}

fn validate_remote_brain_tls_identity(
    certificates: &[CertificateDer<'_>],
    listen: SocketAddr,
    now: UnixTime,
    crypto_provider: &rustls::crypto::CryptoProvider,
) -> Result<()> {
    let leaf = certificates
        .first()
        .ok_or_else(|| TraceDecayError::Config {
            message: "Remote Brain TLS certificate chain is empty".to_owned(),
        })?;
    if certificates.len() < 2 {
        return Err(TraceDecayError::Config {
            message: "Remote Brain TLS certificate chain requires a leaf followed by an explicit trust anchor".to_owned(),
        });
    }
    let mut roots = rustls::RootCertStore::empty();
    let trust_anchor = &certificates[certificates.len() - 1];
    if leaf.as_ref() == trust_anchor.as_ref() {
        return Err(TraceDecayError::Config {
            message:
                "Remote Brain TLS leaf and explicit trust anchor must be distinct certificates"
                    .to_owned(),
        });
    }
    roots.add(trust_anchor.clone()).map_err(|error| {
        tls_configuration_error("load Remote Brain TLS chain trust anchor", error)
    })?;
    let intermediates = &certificates[1..certificates.len() - 1];
    let parsed_leaf = ParsedCertificate::try_from(leaf).map_err(|error| {
        tls_configuration_error("parse Remote Brain TLS leaf certificate", error)
    })?;
    let server_name = ServerName::IpAddress(listen.ip().into());
    verify_server_cert_signed_by_trust_anchor(
        &parsed_leaf,
        &roots,
        intermediates,
        now,
        crypto_provider.signature_verification_algorithms.all,
    )
    .map_err(|error| {
        tls_configuration_error("validate Remote Brain TLS certificate chain", error)
    })?;
    verify_server_name(&parsed_leaf, &server_name).map_err(|error| {
        tls_configuration_error("validate Remote Brain TLS listen address identity", error)
    })?;
    Ok(())
}

#[cfg(test)]
pub(super) fn validate_remote_brain_tls_identity_at(
    certificates: &[CertificateDer<'_>],
    listen: SocketAddr,
    now: UnixTime,
) -> Result<()> {
    validate_remote_brain_tls_identity(
        certificates,
        listen,
        now,
        &rustls::crypto::ring::default_provider(),
    )
}

async fn serve_remote_brain_tls(
    listener: RemoteBrainTlsListener,
    router: Router,
    mut shutdown_requested: oneshot::Receiver<()>,
) -> Result<()> {
    let graceful = CancellationToken::new();
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown_requested => break,
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(joined) = joined {
                    observe_remote_brain_connection_join(joined);
                }
            }
            accepted = listener.accept() => {
                if let Some((io, address)) = accepted {
                    let router = router.clone();
                    let graceful = graceful.clone();
                    connections.spawn(async move {
                        serve_remote_brain_tls_connection(io, router, graceful, address).await;
                    });
                }
            }
        }
    }

    graceful.cancel();
    let drain = async {
        while let Some(joined) = connections.join_next().await {
            observe_remote_brain_connection_join(joined);
        }
    };
    if tokio::time::timeout(REMOTE_BRAIN_TLS_SHUTDOWN_DRAIN_DEADLINE, drain)
        .await
        .is_err()
    {
        connections.abort_all();
        while let Some(joined) = connections.join_next().await {
            observe_remote_brain_connection_join(joined);
        }
    }
    Ok(())
}

async fn serve_remote_brain_tls_connection(
    io: RemoteBrainTlsIo,
    router: Router,
    graceful: CancellationToken,
    address: SocketAddr,
) {
    let service =
        router.map_request(|request: hyper::Request<hyper::body::Incoming>| request.map(Body::new));
    let service = TowerToHyperService::new(service);
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder.keep_alive(false);
    let connection = builder.serve_connection(TokioIo::new(io), service);
    tokio::pin!(connection);
    let result = tokio::select! {
        result = &mut connection => result,
        () = graceful.cancelled() => {
            connection.as_mut().graceful_shutdown();
            connection.await
        }
    };
    if let Err(error) = result {
        tracing::debug!(%error, %address, "Remote Brain TLS connection stopped");
    }
}

fn observe_remote_brain_connection_join(joined: std::result::Result<(), tokio::task::JoinError>) {
    if let Err(error) = joined
        && !error.is_cancelled()
    {
        tracing::warn!(%error, "Remote Brain TLS connection task failed");
    }
}

enum RemoteBrainTlsTransport {
    Handshaking(Pin<Box<tokio_rustls::Accept<tokio::net::TcpStream>>>),
    Streaming(Box<tokio_rustls::server::TlsStream<tokio::net::TcpStream>>),
    Failed,
}

struct RemoteBrainTlsIo {
    transport: RemoteBrainTlsTransport,
    read_idle_deadline: Pin<Box<tokio::time::Sleep>>,
    request_read_deadline: Pin<Box<tokio::time::Sleep>>,
    write_idle_deadline: Pin<Box<tokio::time::Sleep>>,
    response_deadline: Pin<Box<tokio::time::Sleep>>,
    close_deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    egress_started: bool,
    #[cfg(test)]
    egress_write_pending: bool,
    #[cfg(test)]
    egress_flush_pending: bool,
    #[cfg(test)]
    egress_idle_expired: bool,
    #[cfg(test)]
    egress_response_expired: bool,
    #[cfg(test)]
    egress_last_positive_write: Option<tokio::time::Instant>,
    #[cfg(test)]
    egress_write_idle_deadline_at: Option<tokio::time::Instant>,
    #[cfg(test)]
    egress: Arc<RemoteBrainTlsEgressObserver>,
    header_bytes: Vec<u8>,
    header_terminator_prefix: usize,
    http2_preface_prefix: Option<usize>,
    headers_complete: bool,
    request_body_remaining: u64,
    request_read_complete: bool,
    _permit: OwnedSemaphorePermit,
}

impl RemoteBrainTlsIo {
    fn new(
        handshake: tokio_rustls::Accept<tokio::net::TcpStream>,
        permit: OwnedSemaphorePermit,
        #[cfg(test)] egress: Arc<RemoteBrainTlsEgressObserver>,
    ) -> Self {
        Self {
            transport: RemoteBrainTlsTransport::Handshaking(Box::pin(handshake)),
            read_idle_deadline: Box::pin(tokio::time::sleep(REMOTE_BRAIN_TLS_READ_IDLE_DEADLINE)),
            request_read_deadline: Box::pin(tokio::time::sleep(
                REMOTE_BRAIN_TLS_REQUEST_READ_DEADLINE,
            )),
            write_idle_deadline: Box::pin(tokio::time::sleep(REMOTE_BRAIN_TLS_WRITE_IDLE_DEADLINE)),
            response_deadline: Box::pin(tokio::time::sleep(REMOTE_BRAIN_TLS_RESPONSE_DEADLINE)),
            close_deadline: None,
            egress_started: false,
            #[cfg(test)]
            egress_write_pending: false,
            #[cfg(test)]
            egress_flush_pending: false,
            #[cfg(test)]
            egress_idle_expired: false,
            #[cfg(test)]
            egress_response_expired: false,
            #[cfg(test)]
            egress_last_positive_write: None,
            #[cfg(test)]
            egress_write_idle_deadline_at: None,
            #[cfg(test)]
            egress,
            header_bytes: Vec::new(),
            header_terminator_prefix: 0,
            http2_preface_prefix: Some(0),
            headers_complete: false,
            request_body_remaining: 0,
            request_read_complete: false,
            _permit: permit,
        }
    }

    fn poll_request_absolute_deadline(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        if self.request_read_complete {
            return Ok(());
        }
        if self.request_read_deadline.as_mut().poll(context).is_ready() {
            self.transport = RemoteBrainTlsTransport::Failed;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Remote Brain TLS request admission timed out",
            ));
        }
        Ok(())
    }

    fn poll_read_idle_deadline(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        if self.request_read_complete {
            return Ok(());
        }
        if self.read_idle_deadline.as_mut().poll(context).is_pending() {
            return Ok(());
        }
        self.transport = RemoteBrainTlsTransport::Failed;
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Remote Brain TLS request read timed out",
        ))
    }

    fn poll_write_deadline(
        &mut self,
        context: &mut Context<'_>,
        start_egress: bool,
    ) -> io::Result<()> {
        if !self.egress_started && start_egress {
            let now = tokio::time::Instant::now();
            self.write_idle_deadline
                .as_mut()
                .reset(now + REMOTE_BRAIN_TLS_WRITE_IDLE_DEADLINE);
            self.response_deadline
                .as_mut()
                .reset(now + REMOTE_BRAIN_TLS_RESPONSE_DEADLINE);
            self.egress_started = true;
            #[cfg(test)]
            {
                self.egress_last_positive_write = Some(now);
                self.egress_write_idle_deadline_at =
                    Some(now + REMOTE_BRAIN_TLS_WRITE_IDLE_DEADLINE);
                self.egress.active.fetch_add(1, Ordering::SeqCst);
            }
        }
        if !self.egress_started {
            return Ok(());
        }
        let write_idle_expired = self.write_idle_deadline.as_mut().poll(context).is_ready();
        let response_expired = self.response_deadline.as_mut().poll(context).is_ready();
        if !write_idle_expired && !response_expired {
            return Ok(());
        }
        self.transport = RemoteBrainTlsTransport::Failed;
        #[cfg(test)]
        if write_idle_expired && !self.egress_idle_expired {
            self.egress_idle_expired = true;
            self.egress.idle_expirations.fetch_add(1, Ordering::SeqCst);
            let violates_idle_deadline = self
                .egress_last_positive_write
                .zip(self.egress_write_idle_deadline_at)
                .is_none_or(|(last_write, deadline_at)| {
                    deadline_at.saturating_duration_since(last_write)
                        != std::time::Duration::from_secs(5)
                });
            if violates_idle_deadline {
                self.egress
                    .idle_deadline_contract_violations
                    .fetch_add(1, Ordering::SeqCst);
            }
        }
        #[cfg(test)]
        if response_expired && !self.egress_response_expired {
            self.egress_response_expired = true;
            self.egress
                .response_expirations
                .fetch_add(1, Ordering::SeqCst);
        }
        #[cfg(test)]
        self.clear_egress_backpressure();
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Remote Brain TLS response write timed out",
        ))
    }

    fn reset_write_idle_deadline(&mut self) {
        let now = tokio::time::Instant::now();
        self.write_idle_deadline
            .as_mut()
            .reset(now + REMOTE_BRAIN_TLS_WRITE_IDLE_DEADLINE);
        #[cfg(test)]
        {
            self.egress_last_positive_write = Some(now);
            self.egress_write_idle_deadline_at = Some(now + REMOTE_BRAIN_TLS_WRITE_IDLE_DEADLINE);
        }
    }

    fn poll_close_deadline(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let deadline = self
            .close_deadline
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(REMOTE_BRAIN_TLS_CLOSE_DEADLINE)));
        if deadline.as_mut().poll(context).is_pending() {
            Poll::Pending
        } else {
            self.transport = RemoteBrainTlsTransport::Failed;
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Remote Brain TLS close notification timed out",
            )))
        }
    }

    fn poll_handshake(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let RemoteBrainTlsTransport::Handshaking(handshake) = &mut self.transport else {
            return Poll::Ready(Ok(()));
        };
        match handshake.as_mut().poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(stream)) => {
                self.transport = RemoteBrainTlsTransport::Streaming(Box::new(stream));
                let now = tokio::time::Instant::now();
                self.read_idle_deadline
                    .as_mut()
                    .reset(now + REMOTE_BRAIN_TLS_READ_IDLE_DEADLINE);
                self.request_read_deadline
                    .as_mut()
                    .reset(now + REMOTE_BRAIN_TLS_REQUEST_READ_DEADLINE);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                self.transport = RemoteBrainTlsTransport::Failed;
                Poll::Ready(Err(error))
            }
        }
    }

    fn observe_http_request(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.request_read_complete {
            return Ok(());
        }
        const TERMINATOR: &[u8] = b"\r\n\r\n";
        const HTTP2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
        let mut body_offset = 0;
        if !self.headers_complete {
            for (offset, byte) in bytes.iter().enumerate() {
                self.header_bytes.push(*byte);
                if self.header_bytes.len() > MAX_REMOTE_BRAIN_TLS_HEADER_BYTES {
                    return self.fail_request(
                        io::ErrorKind::InvalidData,
                        "Remote Brain TLS request headers exceed the admission bound",
                    );
                }
                if let Some(prefix) = self.http2_preface_prefix {
                    if *byte == HTTP2_PREFACE[prefix] {
                        let prefix = prefix + 1;
                        if prefix == HTTP2_PREFACE.len() {
                            return self.fail_request(
                                io::ErrorKind::InvalidData,
                                "Remote Brain TLS listener accepts HTTP/1.1 only",
                            );
                        }
                        self.http2_preface_prefix = Some(prefix);
                    } else {
                        self.http2_preface_prefix = None;
                    }
                }
                if *byte == TERMINATOR[self.header_terminator_prefix] {
                    self.header_terminator_prefix += 1;
                    if self.header_terminator_prefix == TERMINATOR.len() {
                        if self.http2_preface_prefix.is_some() {
                            self.header_terminator_prefix = 0;
                            continue;
                        }
                        self.headers_complete = true;
                        #[cfg(test)]
                        self.egress.headers_complete.fetch_add(1, Ordering::SeqCst);
                        body_offset = offset + 1;
                        break;
                    }
                } else {
                    self.header_terminator_prefix = usize::from(*byte == TERMINATOR[0]);
                }
            }
            if !self.headers_complete {
                return Ok(());
            }
            self.request_body_remaining = match declared_http11_body_length(&self.header_bytes) {
                Ok(length) => length,
                Err(error) => {
                    self.transport = RemoteBrainTlsTransport::Failed;
                    return Err(error);
                }
            };
            self.header_bytes = Vec::new();
        }

        let available_body =
            u64::try_from(bytes.len().saturating_sub(body_offset)).unwrap_or(u64::MAX);
        if available_body > self.request_body_remaining {
            return self.fail_request(
                io::ErrorKind::InvalidData,
                "Remote Brain TLS connection carried bytes after its declared request body",
            );
        }
        self.request_body_remaining -= available_body;
        #[cfg(test)]
        self.egress.body_bytes_observed.fetch_add(
            usize::try_from(available_body).unwrap_or(usize::MAX),
            Ordering::SeqCst,
        );
        self.request_read_complete = self.request_body_remaining == 0;
        Ok(())
    }

    fn fail_request(&mut self, kind: io::ErrorKind, message: &'static str) -> io::Result<()> {
        self.transport = RemoteBrainTlsTransport::Failed;
        Err(io::Error::new(kind, message))
    }

    fn failed_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "Remote Brain TLS handshake failed",
        )
    }
}

impl AsyncRead for RemoteBrainTlsIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Err(error) = self.poll_request_absolute_deadline(context) {
            return Poll::Ready(Err(error));
        }
        match self.poll_handshake(context) {
            Poll::Pending => {
                return match self.poll_read_idle_deadline(context) {
                    Ok(()) => Poll::Pending,
                    Err(error) => Poll::Ready(Err(error)),
                };
            }
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        let before = buffer.filled().len();
        let result = match &mut self.transport {
            RemoteBrainTlsTransport::Streaming(stream) => {
                Pin::new(stream).poll_read(context, buffer)
            }
            RemoteBrainTlsTransport::Failed => Poll::Ready(Err(Self::failed_error())),
            RemoteBrainTlsTransport::Handshaking(_) => Poll::Pending,
        };
        if matches!(result, Poll::Ready(Ok(()))) {
            if buffer.filled().len() > before {
                self.read_idle_deadline
                    .as_mut()
                    .reset(tokio::time::Instant::now() + REMOTE_BRAIN_TLS_READ_IDLE_DEADLINE);
            }
            if let Err(error) = self.observe_http_request(&buffer.filled()[before..]) {
                return Poll::Ready(Err(error));
            }
        }
        if result.is_pending()
            && let Err(error) = self.poll_read_idle_deadline(context)
        {
            return Poll::Ready(Err(error));
        }
        result
    }
}

impl AsyncWrite for RemoteBrainTlsIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if let Err(error) = self.poll_write_deadline(context, !buffer.is_empty()) {
            return Poll::Ready(Err(error));
        }
        match self.poll_handshake(context) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        let result = match &mut self.transport {
            RemoteBrainTlsTransport::Streaming(stream) => {
                Pin::new(stream).poll_write(context, buffer)
            }
            RemoteBrainTlsTransport::Failed => Poll::Ready(Err(Self::failed_error())),
            RemoteBrainTlsTransport::Handshaking(_) => Poll::Pending,
        };
        if matches!(result, Poll::Ready(Ok(written)) if written > 0) {
            self.reset_write_idle_deadline();
        }
        #[cfg(test)]
        self.observe_egress_write_pending(result.is_pending());
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Err(error) = self.poll_write_deadline(context, false) {
            return Poll::Ready(Err(error));
        }
        match self.poll_handshake(context) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        let result = match &mut self.transport {
            RemoteBrainTlsTransport::Streaming(stream) => Pin::new(stream).poll_flush(context),
            RemoteBrainTlsTransport::Failed => Poll::Ready(Err(Self::failed_error())),
            RemoteBrainTlsTransport::Handshaking(_) => Poll::Pending,
        };
        #[cfg(test)]
        {
            let flush_pending = self.egress_started && result.is_pending();
            self.observe_egress_flush_pending(flush_pending);
        }
        result
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.poll_handshake(context) {
            Poll::Pending => return self.poll_close_deadline(context),
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        let shutdown = match &mut self.transport {
            RemoteBrainTlsTransport::Streaming(stream) => Pin::new(stream).poll_shutdown(context),
            RemoteBrainTlsTransport::Failed => Poll::Ready(Err(Self::failed_error())),
            RemoteBrainTlsTransport::Handshaking(_) => Poll::Pending,
        };
        if !shutdown.is_pending() {
            return shutdown;
        }
        self.poll_close_deadline(context)
    }
}

#[cfg(test)]
impl RemoteBrainTlsIo {
    fn observe_egress_write_pending(&mut self, pending: bool) {
        let was_backpressured = self.egress_write_pending || self.egress_flush_pending;
        self.egress_write_pending = pending;
        self.update_egress_backpressure(was_backpressured);
    }

    fn observe_egress_flush_pending(&mut self, pending: bool) {
        let was_backpressured = self.egress_write_pending || self.egress_flush_pending;
        self.egress_flush_pending = pending;
        self.update_egress_backpressure(was_backpressured);
    }

    fn update_egress_backpressure(&self, was_backpressured: bool) {
        let backpressured = self.egress_write_pending || self.egress_flush_pending;
        if backpressured == was_backpressured {
            return;
        }
        if backpressured {
            self.egress.backpressured.fetch_add(1, Ordering::SeqCst);
        } else {
            self.egress.backpressured.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn clear_egress_backpressure(&mut self) {
        let was_backpressured = self.egress_write_pending || self.egress_flush_pending;
        self.egress_write_pending = false;
        self.egress_flush_pending = false;
        self.update_egress_backpressure(was_backpressured);
    }
}

#[cfg(test)]
impl Drop for RemoteBrainTlsIo {
    fn drop(&mut self) {
        self.clear_egress_backpressure();
        if self.egress_started {
            self.egress.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn declared_http11_body_length(header_bytes: &[u8]) -> io::Result<u64> {
    let headers = std::str::from_utf8(header_bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Remote Brain TLS request headers are invalid",
        )
    })?;
    if !headers.ends_with("\r\n\r\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Remote Brain TLS request headers are incomplete",
        ));
    }
    let mut lines = headers.split("\r\n");
    if lines.next().is_none_or(str::is_empty) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Remote Brain TLS request line is missing",
        ));
    }
    let mut content_length = None;
    for line in lines.take_while(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Remote Brain TLS request header is invalid",
            )
        })?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Remote Brain TLS requests require an explicit content length",
            ));
        }
        if !name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        if content_length.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Remote Brain TLS request repeats content length",
            ));
        }
        let value = value.trim().parse::<u64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Remote Brain TLS request content length is invalid",
            )
        })?;
        content_length = Some(value);
    }
    Ok(content_length.unwrap_or(0))
}

fn tls_configuration_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("failed to {operation}: {error}"),
    }
}
