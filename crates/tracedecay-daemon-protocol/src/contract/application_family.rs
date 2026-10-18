//! Closed Work/Workflow request and result rows shared by their callers.

macro_rules! application_operation_family {
    (
        $(#[$invocation_meta:meta])*
        invocation $invocation:ident;
        $(#[$outcome_meta:meta])*
        outcome $outcome:ident;
        $($variant:ident: $key:literal => $request:ty, $result:ty;)+
    ) => {
        $(#[$invocation_meta])*
        #[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(tag = "operation", content = "request")]
        pub enum $invocation {
            $(#[serde(rename = $key)] $variant($request),)+
        }

        impl $invocation {
            /// Decode only the DTO the caller's admitted operation names.
            pub fn decode(
                operation_key: &str,
                request: serde_json::Value,
            ) -> Result<Self, serde_json::Error> {
                match operation_key {
                    $($key => serde_json::from_value::<$request>(request).map(Self::$variant),)+
                    _ => Err(serde::de::Error::custom(format!(
                        "unknown application operation: {operation_key}"
                    ))),
                }
            }

            pub const fn operation_key(&self) -> &'static str {
                match self { $(Self::$variant(_) => $key,)+ }
            }
        }

        $(#[$outcome_meta])*
        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(tag = "operation", content = "outcome")]
        pub enum $outcome {
            $(#[serde(rename = $key)] $variant($result),)+
        }

        impl $outcome {
            /// Identity used to reject a result from a different operation,
            /// including operations whose payload DTO happens to be shared.
            pub const fn operation_key(&self) -> &'static str {
                match self { $(Self::$variant(_) => $key,)+ }
            }

            /// Erase the payload while retaining typed authority and receipts.
            pub fn into_application_outcome(
                self,
            ) -> Result<tracedecay_contracts::ApplicationOutcome<serde_json::Value>, serde_json::Error> {
                match self {
                    $(Self::$variant(outcome) => outcome.try_map_payload(serde_json::to_value),)+
                }
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::contract::{WorkApplicationInvocationV1, WorkflowApplicationInvocation};

    #[test]
    fn work_recovery_decode_preserves_wire_identity_and_rejects_unknown_fields() {
        let body = json!({"occurred_at": 17});
        let invocation = WorkApplicationInvocationV1::decode("resume_attempts", body.clone())
            .expect("strict recovery request");
        assert_eq!(
            serde_json::to_value(invocation).unwrap(),
            json!({"operation": "resume_attempts", "request": body})
        );
        assert!(
            WorkApplicationInvocationV1::decode(
                "resume_attempts",
                json!({"occurred_at": 17, "unexpected": true}),
            )
            .is_err()
        );
        assert!(WorkApplicationInvocationV1::decode("missing", body).is_err());
    }

    #[test]
    fn workflow_controls_keep_distinct_wire_operations_for_the_same_request_shape() {
        let body = json!({
            "run_id": "run.workflow-decoding",
            "expected_sequence": 7,
            "command_id": "command.workflow-decoding"
        });
        for operation in ["pause_run", "resume_run"] {
            let invocation = WorkflowApplicationInvocation::decode(operation, body.clone())
                .expect("strict run control request");
            assert_eq!(
                serde_json::to_value(invocation).unwrap(),
                json!({"operation": operation, "request": body})
            );
            let mut invalid = body.clone();
            invalid["unexpected"] = json!(true);
            assert!(WorkflowApplicationInvocation::decode(operation, invalid).is_err());
        }
    }
}
