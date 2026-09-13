use std::sync::Arc;

use crate::exact_sql::ExactSqlHandle;

/// A retained exact-SQL capability for one already-authorized store runtime.
///
/// The exact handle never escapes this capability. Its opaque guard retains
/// the issuing database client for as long as any derived repository adapter
/// exists, so an owner cannot retire the physical runtime underneath it.
#[derive(Clone)]
pub struct RetainedExactSqlCapability {
    handle: ExactSqlHandle,
    _guard: Arc<dyn Send + Sync>,
}

impl RetainedExactSqlCapability {
    /// Retains an exact handle together with the client guard that authorized
    /// its use. Callers must supply that guard explicitly; there is no
    /// unguarded or default-retention construction path.
    #[must_use]
    pub fn from_authorized_handle_with_guard<Guard>(handle: ExactSqlHandle, guard: Guard) -> Self
    where
        Guard: Send + Sync + 'static,
    {
        Self {
            handle,
            _guard: Arc::new(guard),
        }
    }

    pub(crate) fn handle(&self) -> &ExactSqlHandle {
        &self.handle
    }
}
