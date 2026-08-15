/// Formats task context as Markdown or JSON.
pub mod formatter;

/// Re-ranking of search candidates using structural signals.
pub mod ranking;

/// Section preview, retrieval handle, and structure for markdown symbols.
pub mod markdown_sections;

/// Cross-session cache backing `tracedecay_read`.
pub mod read_cache;

/// Mode dispatchers (`full`, `lines`, `map`, `signatures`) for `tracedecay_read`.
pub mod read_modes;

/// Shared source-read path, rendering, context, and cache authority.
pub(crate) mod source_read;

pub(crate) use formatter::{
    CONTEXT_CODE_HEADING, CONTEXT_ENTRY_POINTS_HEADING, CONTEXT_EXTENSION_POINTS_HEADING,
    CONTEXT_INDEX_COVERAGE_HINT_HEADING, CONTEXT_MEMORY_FEEDBACK_HINT,
    CONTEXT_MEMORY_MATCHES_HEADING, CONTEXT_PRIORITY_HEADINGS, CONTEXT_RELATED_SYMBOLS_HEADING,
    CONTEXT_SEEN_NODE_IDS_LABEL, CONTEXT_TEST_COVERAGE_HEADING,
};
pub use formatter::{format_context_as_json, format_context_as_markdown};
