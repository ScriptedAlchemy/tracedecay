use serde::{Deserialize, Serialize};

use crate::{ErrorPayload, Output, VerifiedCopiedSnapshot};

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Response {
    pub protocol_version: u16,
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_snapshot: Option<VerifiedCopiedSnapshot>,
    #[serde(flatten)]
    pub outcome: ResponseOutcome,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResponseOutcome {
    Ok { output: Output },
    Error { error: ErrorPayload },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseWire {
    protocol_version: u16,
    request_id: Option<String>,
    #[serde(default)]
    verified_snapshot: Option<VerifiedCopiedSnapshot>,
    status: String,
    #[serde(default)]
    output: Option<Output>,
    #[serde(default)]
    error: Option<ErrorPayload>,
}

impl<'de> Deserialize<'de> for Response {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ResponseWire::deserialize(deserializer)?;
        let outcome = match (wire.status.as_str(), wire.output, wire.error) {
            ("ok", Some(output), None) => ResponseOutcome::Ok { output },
            ("error", None, Some(error)) => ResponseOutcome::Error { error },
            ("ok", _, _) => {
                return Err(serde::de::Error::custom(
                    "an ok response must contain output and no error",
                ));
            }
            ("error", _, _) => {
                return Err(serde::de::Error::custom(
                    "an error response must contain error and no output",
                ));
            }
            (status, _, _) => {
                return Err(serde::de::Error::custom(format!(
                    "unsupported response status {status:?}"
                )));
            }
        };
        Ok(Self {
            protocol_version: wire.protocol_version,
            request_id: wire.request_id,
            verified_snapshot: wire.verified_snapshot,
            outcome,
        })
    }
}
