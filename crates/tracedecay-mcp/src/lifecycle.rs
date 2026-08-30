//! Server-shaped lifecycle observation ports.
//!
//! Concrete daemon lifecycle / shutdown types stay in the composition root.
//! The MCP connection loop observes drain and request admission through this
//! port.

use std::future::Future;
use std::pin::Pin;

/// Outcome of one MCP connection or server shutdown join.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum McpShutdownStatus {
    Clean,
    Failed(String),
    TimedOut,
}

impl McpShutdownStatus {
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        matches!(self, Self::Clean)
    }
}

/// Request-activity guard retained while one MCP request is admitted.
///
/// Dropping the guard releases the underlying lifecycle seat. The boxed
/// retainee is the root-implemented activity token.
pub struct McpRequestActivity {
    _retain: Box<dyn Send>,
}

impl McpRequestActivity {
    pub fn retain<T: Send + 'static>(guard: T) -> Self {
        Self {
            _retain: Box::new(guard),
        }
    }
}

pub type McpLifecycleDrainFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Observe daemon drain and admit one request seat without naming daemon types.
pub trait McpConnectionLifecyclePort: Send + Sync {
    fn accepting(&self) -> bool;
    fn try_enter(&self) -> Option<McpRequestActivity>;
    fn wait_for_draining(&self) -> McpLifecycleDrainFuture<'_>;
}
