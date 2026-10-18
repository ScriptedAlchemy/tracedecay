//! Canonical single-frame JSON output for CLI adapters.

use serde::Serialize;
use serde_json::Value;
use tracedecay_contracts::ApplicationResult;

use super::view::PER_CALL_POINTERS;

/// Serializes one canonical Plan 09 success or problem envelope as exactly one
/// UTF-8 JSON line, without a transport-specific `Ok`/`Err` wrapper.
#[tracing::instrument(name = "mcp.application_output.json_line", level = "trace", skip_all)]
pub fn json_line<T: Serialize>(result: &ApplicationResult<T>) -> serde_json::Result<String> {
    let mut rendered = match result {
        Ok(envelope) => serde_json::to_string(envelope)?,
        Err(envelope) => serde_json::to_string(envelope)?,
    };
    rendered.push('\n');
    Ok(rendered)
}

/// `text` as a canonical JSON envelope without its per-call values, or `None`
/// when `text` is not one.
pub(crate) fn without_per_call_fields(text: &str) -> Option<String> {
    let mut envelope = serde_json::from_str::<Value>(text).ok()?;
    if !envelope.get("contract").is_some_and(Value::is_object)
        || !envelope.get("request_id").is_some_and(Value::is_string)
    {
        return None;
    }
    let packet = envelope.pointer("/outcome/outcome") != Some(&Value::from("result"));
    for pointer in PER_CALL_POINTERS {
        if !packet && pointer.starts_with("/outcome/value/") {
            continue;
        }
        let (parent, field) = pointer.rsplit_once('/')?;
        if let Some(parent) = envelope.pointer_mut(parent).and_then(Value::as_object_mut) {
            parent.remove(field);
        }
    }
    Some(envelope.to_string())
}

#[cfg(test)]
mod tests {
    use super::json_line;
    use tracedecay_contracts::{
        ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult, RequestId,
        ResultContractRef, RetryDirective,
    };
    use tracedecay_tool_catalog::SchemaId;

    #[test]
    fn emits_one_unwrapped_canonical_json_line() {
        let result: ApplicationResult<()> = Err(ApplicationProblemEnvelope::new(
            ResultContractRef::new(SchemaId::new("schema.test.result").unwrap(), 1).unwrap(),
            RequestId::new("request.test").unwrap(),
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        )
        .expect("canonical JSON problem fixture"));
        let rendered = json_line(&result).unwrap();

        assert!(rendered.ends_with('\n'));
        assert_eq!(rendered.lines().count(), 1);
        assert!(!rendered.contains("\"Err\""));
        assert!(rendered.contains("\"not_found_or_not_authorized\""));
    }
}
