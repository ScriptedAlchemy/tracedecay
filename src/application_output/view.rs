use serde::Serialize;
use serde_json::Value;
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblemRecord, ApplicationResult, EvidenceCoverage, Omission,
    OperationReceipt, ResolvedScope,
};
use tracedecay_tool_catalog::BindingId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HumanFieldValue {
    Code(String),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HumanField {
    pub label: &'static str,
    pub value: HumanFieldValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalHumanView {
    pub(crate) heading: String,
    pub(crate) fields: Vec<HumanField>,
}

impl CanonicalHumanView {
    pub fn from_application_result(
        operation: &str,
        binding_id: &BindingId,
        result: &ApplicationResult<Value>,
    ) -> serde_json::Result<Self> {
        let mut view = Self {
            heading: operation.to_owned(),
            fields: Vec::new(),
        };
        view.code("Operation", operation);
        view.code("Binding", binding_id.as_str());

        match result {
            Ok(envelope) => {
                view.code("Status", "success");
                view.code(
                    "Contract",
                    format!(
                        "{}@{}",
                        envelope.contract.schema_id().as_str(),
                        envelope.contract.schema_revision()
                    ),
                );
                view.code("Request", envelope.request_id.as_str());
                view.push_scope(&envelope.scope)?;
                match &envelope.outcome {
                    ApplicationOutcome::Evidence(packet) => {
                        view.code("Outcome", "evidence");
                        view.code("Freshness", scalar(&packet.temporal.freshness)?);
                        view.push_coverage(&packet.coverage)?;
                        view.push_omissions(&packet.omissions)?;
                        let cursor = packet
                            .page
                            .cursor
                            .as_ref()
                            .map(scalar)
                            .transpose()?
                            .unwrap_or_else(|| "none".to_owned());
                        view.code("Cursor", cursor);
                        view.code("Page returned", packet.page.returned.to_string());
                        view.code(
                            "Page total",
                            packet
                                .page
                                .total
                                .map_or_else(|| "unknown".to_owned(), |total| total.to_string()),
                        );
                        view.push_receipt(&packet.execution)?;
                        view.text("Payload", payload_summary(packet.payload.as_ref())?);
                    }
                    ApplicationOutcome::Preview(preview) => {
                        view.code("Outcome", "preview");
                        view.code("Preview", preview.preview_id.as_str());
                        view.code("Preview digest", scalar(&preview.preview_digest)?);
                        view.code("Effect class", scalar(&preview.effect_class)?);
                        view.code("Expected state", scalar(&preview.expected_state)?);
                        view.push_receipt(&preview.execution)?;
                        view.text("Payload", payload_summary(preview.payload.as_ref())?);
                    }
                    ApplicationOutcome::Effect(effect) => {
                        view.code("Outcome", "effect");
                        view.code("Effect", effect.effect_id.as_str());
                        view.code("Effect class", scalar(&effect.effect_class)?);
                        view.code("Idempotency key", effect.idempotency_key.as_str());
                        view.code("Expected state", scalar(&effect.expected_state)?);
                        view.code("Reconciliation", scalar(&effect.reconciliation)?);
                        view.push_receipt(&effect.execution)?;
                        view.code("Receipt operation", scalar(&effect.receipt.operation)?);
                        view.code("Receipt outcome", scalar(&effect.receipt.outcome)?);
                        view.code("Receipt actor", scalar(&effect.receipt.actor)?);
                        view.text("Payload", payload_summary(effect.payload.as_ref())?);
                    }
                }
            }
            Err(envelope) => {
                view.code("Status", "problem");
                view.code(
                    "Contract",
                    format!(
                        "{}@{}",
                        envelope.contract.schema_id().as_str(),
                        envelope.contract.schema_revision()
                    ),
                );
                view.push_problem(&envelope.problem)?;
            }
        }
        Ok(view)
    }

    fn push_scope(&mut self, scope: &ResolvedScope) -> serde_json::Result<()> {
        self.code("Scope project", scalar(&scope.project_id)?);
        self.code("Scope repository", scalar(&scope.repository_id)?);
        self.code("Scope worktree", scalar(&scope.worktree_id)?);
        self.code(
            "Scope reference",
            scope
                .reference
                .as_ref()
                .map(scalar)
                .transpose()?
                .unwrap_or_else(|| "none".to_owned()),
        );
        self.code("Scope digest", scalar(&scope.scope_digest)?);
        Ok(())
    }

    fn push_coverage(&mut self, coverage: &EvidenceCoverage) -> serde_json::Result<()> {
        self.code("Coverage", scalar(&coverage.completeness)?);
        self.code(
            "Coverage counts",
            format!(
                "visited={}, eligible={}, returned={}",
                optional_count(coverage.visited),
                optional_count(coverage.eligible),
                coverage.returned
            ),
        );
        let domains = coverage
            .domains
            .iter()
            .map(|domain| {
                Ok(format!(
                    "{}:{}",
                    scalar(&domain.domain)?,
                    scalar(&domain.completeness)?
                ))
            })
            .collect::<serde_json::Result<Vec<_>>>()?;
        self.code("Coverage domains", list_or_none(domains));
        Ok(())
    }

    fn push_omissions(&mut self, omissions: &[Omission]) -> serde_json::Result<()> {
        let omissions = omissions
            .iter()
            .map(|omission| {
                Ok(format!(
                    "{}:{}={}",
                    scalar(&omission.domain)?,
                    scalar(&omission.reason)?,
                    omission.count
                ))
            })
            .collect::<serde_json::Result<Vec<_>>>()?;
        self.code("Omissions", list_or_none(omissions));
        Ok(())
    }

    fn push_receipt(&mut self, receipt: &OperationReceipt) -> serde_json::Result<()> {
        self.code("Termination", scalar(&receipt.termination)?);
        self.code(
            "Receipt",
            format!(
                "started={}, ended={}, deadline={}, units={}, bytes={}, elapsed_us={}",
                receipt.started_at.0,
                receipt.ended_at.0,
                receipt.effective_deadline.expires_at.0,
                receipt.budget.units_consumed,
                receipt.budget.bytes_consumed,
                receipt.budget.elapsed_micros
            ),
        );
        self.code(
            "Cancellation stage",
            receipt
                .cancellation
                .as_ref()
                .map(|observation| scalar(&observation.stage))
                .transpose()?
                .unwrap_or_else(|| "none".to_owned()),
        );
        Ok(())
    }

    fn push_problem(&mut self, problem: &ApplicationProblemRecord) -> serde_json::Result<()> {
        self.code("Problem", &problem.code);
        self.code("Problem kind", scalar(&problem.kind)?);
        self.code("Problem revision", problem.revision.to_string());
        self.code("Owning layer", scalar(&problem.owning_layer)?);
        self.code("Terminality", scalar(&problem.terminality)?);
        self.code("Request", problem.request_id.as_str());
        self.code("Trace", problem.trace_id.as_str());
        self.text("Message", problem.message.clone());
        self.code("Retryable", problem.retryable.to_string());
        self.code("Retry", scalar(&problem.retry)?);
        self.code(
            "Retry scope",
            problem
                .retry_scope
                .as_ref()
                .map(scalar)
                .transpose()?
                .unwrap_or_else(|| "none".to_owned()),
        );
        self.code(
            "Retry after",
            problem
                .retry_after_millis
                .map_or_else(|| "none".to_owned(), |delay| format!("{delay}ms")),
        );
        self.code(
            "Cancellation stage",
            problem
                .cancellation_stage
                .as_ref()
                .map(scalar)
                .transpose()?
                .unwrap_or_else(|| "none".to_owned()),
        );
        let details = problem
            .details
            .iter()
            .map(|detail| format!("{}: {}", detail.code, detail.message))
            .collect::<Vec<_>>();
        self.text("Details", list_or_none(details));
        let legal_actions = problem
            .legal_actions
            .iter()
            .map(scalar)
            .collect::<serde_json::Result<Vec<_>>>()?;
        self.code("Legal actions", list_or_none(legal_actions));
        if let Some(coverage) = &problem.coverage {
            self.push_coverage(coverage)?;
        } else {
            self.code("Coverage", "not_available");
        }
        Ok(())
    }

    fn code(&mut self, label: &'static str, value: impl Into<String>) {
        self.fields.push(HumanField {
            label,
            value: HumanFieldValue::Code(value.into()),
        });
    }

    fn text(&mut self, label: &'static str, value: impl Into<String>) {
        self.fields.push(HumanField {
            label,
            value: HumanFieldValue::Text(value.into()),
        });
    }
}

fn scalar<T: Serialize>(value: &T) -> serde_json::Result<String> {
    Ok(match serde_json::to_value(value)? {
        Value::String(value) => value,
        value => value.to_string(),
    })
}

fn optional_count(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |count| count.to_string())
}

fn list_or_none(values: Vec<String>) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

fn payload_summary(payload: Option<&Value>) -> serde_json::Result<String> {
    let Some(payload) = payload else {
        return Ok("none".to_owned());
    };
    let bytes = serde_json::to_vec(payload)?.len();
    Ok(match payload {
        Value::Null => "null".to_owned(),
        Value::Bool(_) | Value::Number(_) => payload.to_string(),
        Value::String(value) => {
            format!(
                "string(chars={}, json_bytes={bytes}); complete: --json",
                value.chars().count()
            )
        }
        Value::Array(values) => {
            format!(
                "array(items={}, json_bytes={bytes}); complete: --json",
                values.len()
            )
        }
        Value::Object(values) => {
            let mut keys = values.keys().map(String::as_str).collect::<Vec<_>>();
            keys.sort_unstable();
            let visible = keys.into_iter().take(8).collect::<Vec<_>>();
            let suffix = if values.len() > visible.len() {
                ", …"
            } else {
                ""
            };
            format!(
                "object(keys={}{}; json_bytes={bytes}); complete: --json",
                visible.join(","),
                suffix
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_application::{
        CancellationObservation, CancellationStage, CoverageCompleteness, CoverageDomainState,
        Deadline, EvidenceCoverage, EvidenceDomain, Omission, OmissionReason, OperationBudgetUsage,
        OperationReceipt, OperationTermination,
    };
    use tracedecay_domain::UtcMicros;

    use super::super::markdown;
    use super::{CanonicalHumanView, payload_summary};

    #[test]
    fn partial_evidence_markdown_matches_the_golden_contract() {
        let mut view = CanonicalHumanView {
            heading: "feedback_list".to_owned(),
            fields: Vec::new(),
        };
        view.push_coverage(&EvidenceCoverage {
            requested_domains: vec![EvidenceDomain::Source, EvidenceDomain::Test],
            visited: Some(5),
            eligible: Some(4),
            returned: 2,
            completeness: CoverageCompleteness::Partial,
            domains: vec![
                CoverageDomainState {
                    domain: EvidenceDomain::Source,
                    completeness: CoverageCompleteness::Partial,
                },
                CoverageDomainState {
                    domain: EvidenceDomain::Test,
                    completeness: CoverageCompleteness::Unknown,
                },
            ],
        })
        .unwrap();
        view.push_omissions(&[Omission {
            domain: EvidenceDomain::Source,
            count: 2,
            reason: OmissionReason::Budget,
        }])
        .unwrap();
        view.code("Cursor", "cursor.opaque");
        view.push_receipt(&OperationReceipt {
            started_at: UtcMicros(10),
            ended_at: UtcMicros(20),
            effective_deadline: Deadline::new(UtcMicros(30)).unwrap(),
            cancellation: Some(CancellationObservation {
                stage: CancellationStage::DuringRead,
                observed_at: UtcMicros(18),
            }),
            budget: OperationBudgetUsage {
                units_consumed: 3,
                bytes_consumed: 40,
                elapsed_micros: 10,
            },
            termination: OperationTermination::Partial,
        })
        .unwrap();
        view.text(
            "Payload",
            payload_summary(Some(&json!({"items": [1, 2]}))).unwrap(),
        );

        assert_eq!(
            markdown::render(view).as_str(),
            concat!(
                "## feedback\\_list\n",
                "\n- Coverage: `partial`",
                "\n- Coverage counts: `visited=5, eligible=4, returned=2`",
                "\n- Coverage domains: `source:partial, test:unknown`",
                "\n- Omissions: `source:budget=2`",
                "\n- Cursor: `cursor.opaque`",
                "\n- Termination: `partial`",
                "\n- Receipt: `started=10, ended=20, deadline=30, units=3, bytes=40, elapsed_us=10`",
                "\n- Cancellation stage: `during_read`",
                "\n- Payload: object(keys=items; json\\_bytes=15); complete: --json",
            )
        );
    }
}
