//! The response-handle capabilities surfaces hold.
//!
//! Records live in the daemon-owned registered store of the admitted project
//! (see `tracedecay_global_db::RegisteredGlobalDb::publish_response_handle`).
//! Two capabilities reach that owner:
//!
//! - [`ResponseHandleStore`] publishes and reads through the registered lease
//!   the daemon admitted for the project; retrieval, feedback, and LSP hold it.
//! - [`ResponseHandlePublisher`] only publishes, through any
//!   [`ResponseHandlePublishPort`]: the store itself inside the daemon, or a
//!   daemon invocation from a client process that renders locally.
//!
//! A surface with neither answers the feature-scoped
//! `handle_storage_unavailable` refusal; it never falls back elsewhere.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_contracts::{CancellationSignal, Deadline};
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::cancellation::CancellationToken;

pub use tracedecay_global_db::{
    RESPONSE_HANDLE_TTL_SECS, ResponseHandleError, ResponseHandleInventory, ResponseHandleLookup,
    ResponseHandleRecord, is_valid_response_handle, response_handle_for,
};

pub type Result<T, E = ResponseHandleError> = std::result::Result<T, E>;

pub type ResponseHandleFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Converts a UTC-micros clock sample to the second resolution the handle store uses.
pub fn micros_to_seconds(value: UtcMicros) -> i64 {
    value.0.div_euclid(1_000_000)
}

/// The admitted project's registered handle store.
#[derive(Clone)]
pub struct ResponseHandleStore {
    database: RegisteredGlobalDbLeaseV1,
}

impl std::fmt::Debug for ResponseHandleStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResponseHandleStore")
    }
}

impl ResponseHandleStore {
    pub fn registered(database: RegisteredGlobalDbLeaseV1) -> Self {
        Self { database }
    }

    pub async fn publish(&self, content: &str, now: i64) -> Result<ResponseHandleRecord> {
        self.database.publish_response_handle(content, now).await
    }

    pub async fn publish_with_control(
        &self,
        content: &str,
        deadline: Deadline,
        cancellation: CancellationToken,
        signal: Option<CancellationSignal>,
    ) -> Result<ResponseHandleRecord> {
        self.database
            .publish_response_handle_with_control(content, deadline, cancellation, signal)
            .await
    }

    pub async fn lookup(&self, handle: &str, now: i64) -> Result<ResponseHandleLookup> {
        self.database.lookup_response_handle(handle, now).await
    }

    /// [`Self::publish`] for synchronous feedback and LSP paths; see
    /// [`ResponseHandlePublisher::publish_blocking`].
    pub fn publish_blocking(&self, content: &str, now: i64) -> Result<ResponseHandleRecord> {
        block_on(self.publish(content, now))
    }

    /// [`Self::lookup`] for synchronous feedback paths.
    pub fn lookup_blocking(&self, handle: &str, now: i64) -> Result<ResponseHandleLookup> {
        block_on(self.lookup(handle, now))
    }

    /// The publish-only capability of this store, for render paths.
    pub fn publisher(&self) -> ResponseHandlePublisher {
        ResponseHandlePublisher::from_port(Arc::new(self.clone()))
    }
}

/// A route that publishes into the registered owner of one admitted namespace.
pub trait ResponseHandlePublishPort: Send + Sync {
    fn publish<'a>(
        &'a self,
        content: &'a str,
        now: i64,
    ) -> ResponseHandleFuture<'a, ResponseHandleRecord>;
}

impl ResponseHandlePublishPort for ResponseHandleStore {
    fn publish<'a>(
        &'a self,
        content: &'a str,
        now: i64,
    ) -> ResponseHandleFuture<'a, ResponseHandleRecord> {
        Box::pin(ResponseHandleStore::publish(self, content, now))
    }
}

/// Publish-only access to the admitted project's handle store.
#[derive(Clone)]
pub struct ResponseHandlePublisher {
    port: Arc<dyn ResponseHandlePublishPort>,
}

impl std::fmt::Debug for ResponseHandlePublisher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResponseHandlePublisher")
    }
}

impl ResponseHandlePublisher {
    pub fn from_port(port: Arc<dyn ResponseHandlePublishPort>) -> Self {
        Self { port }
    }

    pub async fn publish(&self, content: &str, now: i64) -> Result<ResponseHandleRecord> {
        self.port.publish(content, now).await
    }

    /// [`Self::publish`] for the synchronous render path. The wait parks on
    /// the shared native runtime; a context that cannot park (a
    /// current-thread runtime) is a typed store failure, never a guess.
    pub fn publish_blocking(&self, content: &str, now: i64) -> Result<ResponseHandleRecord> {
        block_on(self.publish(content, now))
    }
}

fn block_on<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    tracedecay_runtime_core::native_runtime::block_on(future).map_err(|message| {
        ResponseHandleError::Store(TraceDecayError::Config {
            message: format!("response-handle store wait could not run: {message}"),
        })
    })?
}
