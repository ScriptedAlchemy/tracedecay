//! Typed HTTP controls shared by generated SDK operations.

use reqwest::header::{HeaderMap, HeaderValue};

use crate::client::ClientError;
use crate::operations::{APPLICATION_REQUEST_ID_HEADER, TypedOperation};

const DEADLINE_HEADER: &str = "x-tracedecay-deadline-micros";

/// Per-invocation transport controls accepted by typed operations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OperationRequestOptions {
    /// Absolute UTC deadline in microseconds, forwarded to daemon admission.
    pub deadline_micros: Option<i64>,
    /// Stable caller-owned replay identity required by operations whose
    /// generated request-ID control is `Required`.
    pub request_id: Option<tracedecay_application::RequestId>,
}

pub(crate) fn admit<Operation: TypedOperation>(
    options: &OperationRequestOptions,
) -> Result<Option<tracedecay_application::RequestId>, ClientError> {
    use tracedecay_application::retained_surfaces::SdkRequestIdControlV1;

    match (Operation::REQUEST_ID_CONTROL, options.request_id.as_ref()) {
        (SdkRequestIdControlV1::Required, None) => Err(ClientError::InvalidRequest(format!(
            "{} requires a stable request_id replay handle",
            Operation::OPERATION_ID
        ))),
        (SdkRequestIdControlV1::ServerMinted, Some(_)) => {
            Err(ClientError::InvalidRequest(format!(
                "{} uses a server-minted request ID",
                Operation::OPERATION_ID
            )))
        }
        _ => Ok(options.request_id.clone()),
    }
}

pub(crate) fn apply_http_headers(
    headers: &mut HeaderMap,
    options: OperationRequestOptions,
) -> Result<(), ClientError> {
    if let Some(deadline_micros) = options.deadline_micros {
        if deadline_micros <= 0 {
            return Err(ClientError::InvalidRequest(
                "deadline_micros must be positive".into(),
            ));
        }
        let value = HeaderValue::from_str(&deadline_micros.to_string())
            .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;
        headers.insert(DEADLINE_HEADER, value);
    }
    if let Some(request_id) = options.request_id {
        let value = HeaderValue::from_str(request_id.as_str())
            .map_err(|error| ClientError::InvalidConfiguration(error.to_string()))?;
        headers.insert(APPLICATION_REQUEST_ID_HEADER, value);
    }
    Ok(())
}
