use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::UtcMicros;

use crate::context::RequestContext;
use crate::error::ApplicationContractError;
use crate::handlers::ApplicationOperation;
use crate::result::OperationBudgetUsage;

use super::RetrievalRequestMeta;

pub const MAX_SOURCE_READ_PATH_BYTES: usize = 4_096;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceReadModeV1 {
    Full,
    Lines,
    Map,
    Signatures,
}

impl SourceReadModeV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Lines => "lines",
            Self::Map => "map",
            Self::Signatures => "signatures",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceReadPrimitiveRequest {
    pub file: String,
    pub mode: SourceReadModeV1,
    pub lines: Option<String>,
    pub include_symbols: bool,
    pub meta: RetrievalRequestMeta,
}

impl SourceReadPrimitiveRequest {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        let file_is_valid = !self.file.is_empty()
            && self.file.len() <= MAX_SOURCE_READ_PATH_BYTES
            && !self.file.contains('\0');
        let range_shape_is_valid = match self.mode {
            SourceReadModeV1::Lines => self.lines.is_some(),
            SourceReadModeV1::Full | SourceReadModeV1::Map | SourceReadModeV1::Signatures => {
                self.lines.is_none()
            }
        };
        if file_is_valid && range_shape_is_valid && self.meta.page.cursor.is_none() {
            Ok(())
        } else {
            Err(ApplicationContractError::Inconsistent {
                field: "source read request",
            })
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceReadResultV1 {
    pub file: String,
    pub mode: SourceReadModeV1,
    pub mtime_ns: u64,
    pub digest: String,
    pub token_count: usize,
    pub unchanged: bool,
    pub body: Option<String>,
    pub context: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SourceReadPortOutcome {
    Completed {
        result: SourceReadResultV1,
        finished_at: UtcMicros,
        budget: OperationBudgetUsage,
    },
    Partial {
        result: SourceReadResultV1,
        finished_at: UtcMicros,
        budget: OperationBudgetUsage,
    },
    Failed {
        finished_at: UtcMicros,
        budget: OperationBudgetUsage,
    },
}

pub type SourceReadPortFuture<'a> =
    Pin<Box<dyn Future<Output = SourceReadPortOutcome> + Send + 'a>>;

#[derive(Clone, Copy, Debug)]
pub struct SourceReadPortContext<'a> {
    pub request: &'a RequestContext,
    pub operation: &'a ApplicationOperation,
    pub observed_at: UtcMicros,
}

/// Async application port for compatibility-preserving source reads.
///
/// Implementations must delegate range parsing, rendering, and cache handling
/// to the existing source-read kernel.
pub trait SourceReadPrimitivePort {
    fn source_read<'a>(
        &'a self,
        context: SourceReadPortContext<'a>,
        request: &'a SourceReadPrimitiveRequest,
    ) -> SourceReadPortFuture<'a>;
}
