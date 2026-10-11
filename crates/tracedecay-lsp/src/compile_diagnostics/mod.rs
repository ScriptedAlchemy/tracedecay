//! Compiler diagnostics for the TypeScript producer.
//!
//! The daemon's generation-bound producer discovers each tsconfig
//! ([`tsconfig`]), runs the project's own compiler over it, and parses the
//! report ([`typescript`]) into [`Diagnostic`]s that it publishes against the
//! sealed code-index generation.

pub mod tsconfig;
pub mod typescript;

use serde::Serialize;

/// One diagnostic emitted by a language's type-checker.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    /// Project-relative file path (e.g. `src/lib.ts`).
    pub file: String,
    /// 1-based start line.
    pub line_start: u32,
    /// 1-based inclusive end line. Equal to `line_start` for single-line spans.
    pub line_end: u32,
    /// 1-based start column; `1` when the compiler reported none.
    pub column: u32,
    /// Severity: `"error"` or `"warning"`.
    pub level: String,
    /// Compiler-assigned code (e.g. `"TS2322"`). Empty when the compiler
    /// didn't attach one.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Compiler that reported it, e.g. `"typescript"`.
    pub driver: &'static str,
}

/// Which compiler levels become diagnostics. Advisory levels are dropped
/// because they either double-count a diagnostic that already has its own
/// entry or carry no actionable span.
fn is_diagnostic_level(level: &str) -> bool {
    matches!(level, "error" | "warning")
}
