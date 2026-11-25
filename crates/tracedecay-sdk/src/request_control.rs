//! Typed transport controls shared by generated SDK operations.

/// Per-invocation transport controls accepted by typed HTTP operations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OperationRequestOptions {
    /// Absolute UTC deadline in microseconds, forwarded to daemon admission.
    pub deadline_micros: Option<i64>,
    /// Stable caller-owned replay identity required by operations whose
    /// generated request-ID control is `Required`.
    pub request_id: Option<tracedecay_contracts::RequestId>,
    /// Evidence page size and continuation cursor.
    pub page: Option<PageOptions>,
}

/// Evidence page request: a size in `1..=1000` and/or the opaque cursor a
/// previous page returned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageOptions {
    pub size: Option<u32>,
    pub cursor: Option<String>,
}
