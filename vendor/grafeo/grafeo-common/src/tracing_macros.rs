//! Conditional tracing macros.
//!
//! When the `tracing` feature is enabled these expand to the corresponding
//! [`tracing`] crate macros. When disabled they compile to nothing, giving
//! zero overhead for profiles like `embedded` and `browser`.

/// Zero-sized span guard returned when tracing is disabled.
///
/// Allows `let _span = grafeo_debug_span!(...)` to compile without
/// triggering clippy's `let_unit_value` lint.
pub struct NoopSpan;

/// Enters an info-level span. Returns a guard that keeps the span open.
#[macro_export]
macro_rules! grafeo_info_span {
    ($($arg:tt)*) => {{
        #[cfg(feature = "tracing")]
        { ::tracing::info_span!($($arg)*).entered() }
        #[cfg(not(feature = "tracing"))]
        { $crate::tracing_macros::NoopSpan }
    }};
}

/// Enters a debug-level span.
#[macro_export]
macro_rules! grafeo_debug_span {
    ($($arg:tt)*) => {{
        #[cfg(feature = "tracing")]
        { ::tracing::debug_span!($($arg)*).entered() }
        #[cfg(not(feature = "tracing"))]
        { $crate::tracing_macros::NoopSpan }
    }};
}

/// Emits a warn-level event.
///
/// When the `tracing` feature is disabled, the format string and its arguments
/// are still referenced (via `format_args!`) so that variables captured in the
/// message do not trigger `unused_variable` warnings. The compiler optimises
/// the dead `format_args!` call away entirely.
#[macro_export]
macro_rules! grafeo_warn {
    ($($arg:tt)*) => {{
        #[cfg(feature = "tracing")]
        { ::tracing::warn!($($arg)*); }
        #[cfg(not(feature = "tracing"))]
        { if false { let _ = format_args!($($arg)*); } }
    }};
}

/// Emits an info-level event.
#[macro_export]
macro_rules! grafeo_info {
    ($($arg:tt)*) => {{
        #[cfg(feature = "tracing")]
        { ::tracing::info!($($arg)*); }
        #[cfg(not(feature = "tracing"))]
        { if false { let _ = format_args!($($arg)*); } }
    }};
}

/// Emits a debug-level event.
#[macro_export]
macro_rules! grafeo_debug {
    ($($arg:tt)*) => {{
        #[cfg(feature = "tracing")]
        { ::tracing::debug!($($arg)*); }
        #[cfg(not(feature = "tracing"))]
        { if false { let _ = format_args!($($arg)*); } }
    }};
}

/// Emits an error-level event.
#[macro_export]
macro_rules! grafeo_error {
    ($($arg:tt)*) => {{
        #[cfg(feature = "tracing")]
        { ::tracing::error!($($arg)*); }
        #[cfg(not(feature = "tracing"))]
        { if false { let _ = format_args!($($arg)*); } }
    }};
}
