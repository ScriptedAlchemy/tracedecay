//! Composition-root health handlers that still need daemon-owned store or doctor
//! authorities. Portable graph health reports live in `tracedecay_mcp::handlers::health`.

mod runtime;

pub(crate) use runtime::{collect_database_snapshot, handle_runtime};

use std::time::Duration;

use serde_json::{Value, json};

use crate::tracedecay::TraceDecay;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::support::generic_tool_result;
use tracedecay_mcp::ToolResult;
