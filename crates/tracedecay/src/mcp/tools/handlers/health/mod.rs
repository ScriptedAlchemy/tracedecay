//! Composition-root health handlers that still need daemon or root graph helpers.

mod runtime;
mod test_map;

pub(super) use runtime::handle_runtime;
pub(super) use test_map::{handle_test_map, handle_test_risk};

use std::collections::HashSet;
use std::time::Duration;

use serde_json::{Value, json};

use crate::tracedecay::TraceDecay;
use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::support::{effective_path, generic_tool_result, unique_file_paths};
use tracedecay_mcp::ToolResult;
