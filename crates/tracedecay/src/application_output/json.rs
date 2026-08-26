//! Canonical single-frame JSON output for CLI adapters.

use serde::Serialize;
use tracedecay_application::ApplicationResult;

/// Serializes one canonical Plan 09 success or problem envelope as exactly one
/// UTF-8 JSON line, without a transport-specific `Ok`/`Err` wrapper.
pub fn json_line<T: Serialize>(result: &ApplicationResult<T>) -> serde_json::Result<String> {
    let mut rendered = match result {
        Ok(envelope) => serde_json::to_string(envelope)?,
        Err(envelope) => serde_json::to_string(envelope)?,
    };
    rendered.push('\n');
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::json_line;
    use tracedecay_application::{
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
