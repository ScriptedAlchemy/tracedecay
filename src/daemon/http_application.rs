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

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::header::{AUTHORIZATION, ORIGIN};
use axum::http::{HeaderValue, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use constant_time_eq::constant_time_eq;
use tokio::sync::{Mutex, Semaphore, oneshot};
use tokio::task::JoinHandle;
use tower::ServiceExt;
use tracedecay_domain::ProjectId;

use crate::errors::{Result, TraceDecayError};

const MAX_HTTP_APPLICATION_PROJECT_ROUTERS: usize = 8;
const MAX_HTTP_APPLICATION_COLD_RESOLUTIONS: usize = 8;

type ProjectRouterResolverFuture =
    Pin<Box<dyn Future<Output = Result<Option<Router>>> + Send + 'static>>;
type ProjectRouterResolver =
    Arc<dyn Fn(ProjectId) -> ProjectRouterResolverFuture + Send + Sync + 'static>;

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
}

#[derive(Clone)]
pub(super) struct DaemonHttpApplicationRegistry {
    routers: Arc<Mutex<ProjectRouterCache>>,
    resolver: Arc<SyncRwLock<Option<ProjectRouterResolver>>>,
    resolver_admission: Arc<Semaphore>,
    active: Arc<AtomicBool>,
}

impl Default for DaemonHttpApplicationRegistry {
    fn default() -> Self {
        Self {
            routers: Arc::new(Mutex::new(ProjectRouterCache::default())),
            resolver: Arc::new(SyncRwLock::new(None)),
            resolver_admission: Arc::new(Semaphore::new(MAX_HTTP_APPLICATION_COLD_RESOLUTIONS)),
            active: Arc::new(AtomicBool::new(false)),
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

    async fn resolve(&self, project_id: &str) -> Option<Router> {
        let project_id = ProjectId::new(project_id.to_owned()).ok()?;
        if let Some(router) = self.routers.lock().await.get(project_id.as_str()) {
            return Some(router);
        }
        let resolver = {
            let slot = self.resolver.read().ok()?;
            slot.as_ref().cloned()
        }?;
        let _permit = Arc::clone(&self.resolver_admission)
            .try_acquire_owned()
            .ok()?;
        let router = resolver(project_id.clone()).await.ok().flatten()?;
        self.routers
            .lock()
            .await
            .insert(project_id.as_str().to_owned(), router.clone());
        Some(router)
    }

    fn router(self) -> Router {
        Router::new()
            .route(
                "/projects/{project_id}/application/{*tail}",
                any(dispatch_project_application),
            )
            .with_state(self)
    }

    pub(super) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
}

async fn dispatch_project_application(
    State(registry): State<DaemonHttpApplicationRegistry>,
    Path((project_id, tail)): Path<(String, String)>,
    mut request: Request<Body>,
) -> Response {
    let Some(router) = registry.resolve(&project_id).await else {
        return StatusCode::NOT_FOUND.into_response();
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
        active.store(true, Ordering::Release);
        let app = registry.router().layer(middleware::from_fn_with_state(
            admission.clone(),
            require_local_http_admission,
        ));
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
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
