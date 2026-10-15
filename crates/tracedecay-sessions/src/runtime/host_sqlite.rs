use std::sync::{Arc, Mutex, PoisonError};

#[derive(Clone)]
pub(crate) struct SqliteReadConn {
    inner: Arc<Mutex<rusqlite::Connection>>,
}

impl SqliteReadConn {
    pub(crate) fn new(connection: rusqlite::Connection) -> Self {
        Self {
            inner: Arc::new(Mutex::new(connection)),
        }
    }

    #[tracing::instrument(name = "sessions.host_sqlite.with", level = "trace", skip_all)]
    pub(crate) async fn with<T, F>(&self, body: F) -> Option<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> T + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
            let guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
            body(&guard)
        })
        .await
        .ok()
    }
}
