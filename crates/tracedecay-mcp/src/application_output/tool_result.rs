//! The one tool-result rendering of a settled application call, shared by
//! every MCP tool family and the `tracedecay tool` CLI.

use std::path::Path;

use serde_json::Value;
use tracedecay_contracts::{
    ApplicationProblemEnvelope, ApplicationProblemKind, ApplicationProblemRecord, ApplicationResult,
};
use tracedecay_daemon_protocol::{RequestedOutputFormat, requested_output_format};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingId};

use super::markdown;
use super::view::CanonicalHumanView;
use crate::ToolResult;
use crate::handlers::support::text_tool_result;
use crate::tool_errors::problem_structured_content;
use crate::tools::render::finalize_with_format;
use crate::tools::response_trailers::ResponseTrailer;

/// A daemon's typed refusal of one application call. The whole problem
/// record travels so every surface renders its detail, retry directive, and
/// legal actions rather than a reason code and a sentence.
#[derive(Clone, Debug)]
pub struct ApplicationRefusal {
    pub operation: ApplicationSurfaceOperation,
    pub binding_id: BindingId,
    pub problem: ApplicationProblemEnvelope,
}

impl ApplicationRefusal {
    /// Renders the refusal in the output format `args` requested.
    pub fn render(self, response_handle_root: Option<&Path>, args: &Value) -> Result<ToolResult> {
        render_application_result(
            response_handle_root,
            self.operation.as_str(),
            &self.binding_id,
            &Err(self.problem),
            requested_output_format(args),
        )
    }

    /// The refusal as the error a first-party command returns: the owner's
    /// reason code, retryability, and typed detail or diagnostic.
    pub fn into_error(self) -> TraceDecayError {
        problem_error(self.problem)
    }
}

/// An owner's refusal as the error its first-party caller returns: the
/// owner's reason code, retryability, and typed detail or diagnostic.
pub fn problem_error(problem: ApplicationProblemEnvelope) -> TraceDecayError {
    let record = *problem.problem;
    let (reason_code, message) = match record.diagnostic {
        Some(diagnostic) => (diagnostic.code, diagnostic.message),
        None => (record.code, record.message),
    };
    match record.detail {
        Some(detail) => {
            TraceDecayError::project_route_with_detail(reason_code, record.retryable, detail)
        }
        None => TraceDecayError::project_route(reason_code, record.retryable, message),
    }
}

/// Renders one settled application call. A problem is a semantic failure
/// whose whole record rides beside the text as MCP
/// `structuredContent.problem`, which `--json` prints with the result.
pub fn render_application_result(
    response_handle_root: Option<&Path>,
    operation: &str,
    binding_id: &BindingId,
    result: &ApplicationResult<Value>,
    requested_format: RequestedOutputFormat,
) -> Result<ToolResult> {
    let value = match result {
        Ok(application) => serde_json::to_value(application)?,
        Err(problem) => serde_json::to_value(problem)?,
    };
    let markdown = match requested_format {
        RequestedOutputFormat::Json => None,
        RequestedOutputFormat::Markdown => {
            let view = CanonicalHumanView::from_application_result(operation, binding_id, result)?;
            Some(markdown::render(view).as_str().to_owned())
        }
    };
    let text = finalize_with_format(response_handle_root, requested_format, &value, || {
        markdown.unwrap_or_default()
    });
    match result {
        Ok(envelope) => {
            let mut rendered = text_tool_result(&text, Vec::new()).with_structured_result(value);
            ResponseTrailer {
                touched_files: &envelope.touched_files,
                code_graph: envelope.code_graph.as_ref(),
                cost: envelope.cost.as_ref(),
            }
            .attach(&mut rendered);
            Ok(rendered)
        }
        Err(problem) => problem_tool_result(&text, &problem.problem),
    }
}

/// The one refusing tool result: `text` beside the typed record at
/// `structuredContent.problem`, marked as a semantic failure. Markdown alone
/// would strand the problem in prose no client can classify; the legal
/// actions, retry directive, detail, and any committed receipt are what a
/// caller acts on.
pub fn problem_tool_result(text: &str, problem: &ApplicationProblemRecord) -> Result<ToolResult> {
    let failure_message = match problem.kind {
        ApplicationProblemKind::NotFoundOrNotAuthorized => {
            "application surface was not found or is not authorized"
        }
        ApplicationProblemKind::Unavailable => "application surface unavailable",
        _ => "application surface request failed",
    };
    let mut rendered = text_tool_result(text, Vec::new());
    if let Some(object) = rendered.value.as_object_mut() {
        object.insert(
            "structuredContent".to_string(),
            problem_structured_content(problem)?,
        );
    }
    Ok(rendered
        .with_semantic_error(true)
        .with_failure_message(failure_message))
}
