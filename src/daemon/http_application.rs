//! Daemon-owned loopback HTTP lifecycle for canonical application routers.
//!
//! This is intentionally independent of the optional dashboard server. The
//! outer service owns only local transport admission and project routing;
//! every mounted inner router remains the canonical application adapter.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::RwLock as SyncRwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Json, Path, Request, State};
use axum::http::header::{AUTHORIZATION, ORIGIN};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, post};
use constant_time_eq::constant_time_eq;
use serde::Deserialize;
use tokio::sync::{Mutex, Semaphore, oneshot};
use tokio::task::JoinHandle;
use tower::ServiceExt;
use tracedecay_application::remote::auth::RemoteEnrollmentAdmissionEvidenceV1;
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

pub(super) struct DaemonHttpApplicationService {
    endpoint: SocketAddr,
    #[cfg(test)]
    origin: String,
    active: Arc<AtomicBool>,
    remote_credentials: Option<Arc<super::remote_protocol::DaemonRemoteCredentialAuthorityV1>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<()>>>,
}

impl DaemonHttpApplicationService {
    pub(super) async fn bind(
        registry: DaemonHttpApplicationRegistry,
        auth_token: &str,
    ) -> Result<Self> {
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
        Ok(Self {
            endpoint,
            #[cfg(test)]
            origin: origin.to_owned(),
            active,
            remote_credentials,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }

    pub(super) fn endpoint(&self) -> SocketAddr {
        self.endpoint
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
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        task.await.map_err(|error| TraceDecayError::Config {
            message: format!("daemon HTTP application service task failed: {error}"),
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
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
