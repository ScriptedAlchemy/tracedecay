use serde::Serialize;
use serde_json::Value;
use tracedecay_contracts::{
    ApplicationOutcome, ApplicationProblemRecord, ApplicationResult, EvidenceCoverage,
    OperationReceipt, ResolvedScope,
};
use tracedecay_tool_catalog::BindingId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HumanFieldValue {
    Block(String),
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

        match result {
            Ok(envelope) => match &envelope.outcome {
                ApplicationOutcome::Evidence(packet) => {
                    view.block(
                        "Payload",
                        payload_preview(operation, packet.payload.as_ref())?,
                    );
                    view.code("Status", "success");
                    view.push_evidence_summary(packet)?;
                    view.push_provenance(
                        binding_id,
                        envelope.request_id.as_str(),
                        &envelope.scope,
                        packet.execution.budget.elapsed_micros,
                    )?;
                }
                ApplicationOutcome::Preview(preview) => {
                    view.block(
                        "Payload",
                        payload_preview(operation, preview.payload.as_ref())?,
                    );
                    view.code("Status", "success");
                    view.code("Operation", operation);
                    view.code("Binding", binding_id.as_str());
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
                    view.code("Outcome", "preview");
                    view.code("Preview", preview.preview_id.as_str());
                    view.code("Preview digest", scalar(&preview.preview_digest)?);
                    view.code("Effect class", scalar(&preview.effect_class)?);
                    view.code("Expected state", scalar(&preview.expected_state)?);
                    view.push_receipt(&preview.execution)?;
                }
                ApplicationOutcome::Effect(effect) => {
                    view.block(
                        "Payload",
                        payload_preview(operation, effect.payload.as_ref())?,
                    );
                    view.code("Status", "success");
                    view.code("Operation", operation);
                    view.code("Binding", binding_id.as_str());
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
                }
            },
            Err(envelope) => {
                view.code("Operation", operation);
                view.code("Binding", binding_id.as_str());
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

    fn push_evidence_summary(
        &mut self,
        packet: &tracedecay_contracts::EvidencePacket<Value>,
    ) -> serde_json::Result<()> {
        let coverage = &packet.coverage;
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
        let omissions = packet
            .omissions
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
        let cursor = packet
            .page
            .cursor
            .as_ref()
            .map(scalar)
            .transpose()?
            .unwrap_or_else(|| "none".to_owned());
        let cancellation = packet
            .execution
            .cancellation
            .as_ref()
            .map(|observation| scalar(&observation.stage))
            .transpose()?
            .unwrap_or_else(|| "none".to_owned());
        self.code(
            "Evidence",
            format!(
                "freshness={}; coverage={}; visited={}; eligible={}; returned={}; total={}; domains={}; omissions={}; cursor={}; termination={}; cancellation={}",
                scalar(&packet.temporal.freshness)?,
                scalar(&coverage.completeness)?,
                optional_count(coverage.visited),
                optional_count(coverage.eligible),
                coverage.returned,
                packet
                    .page
                    .total
                    .map_or_else(|| "unknown".to_owned(), |total| total.to_string()),
                list_or_none(&domains),
                list_or_none(&omissions),
                cursor,
                scalar(&packet.execution.termination)?,
                cancellation,
            ),
        );
        Ok(())
    }

    fn push_provenance(
        &mut self,
        binding_id: &BindingId,
        request_id: &str,
        scope: &ResolvedScope,
        elapsed_micros: u64,
    ) -> serde_json::Result<()> {
        self.code(
            "Provenance",
            format!(
                "binding={}; request={}; project={}; worktree={}; elapsed_us={elapsed_micros}",
                binding_id.as_str(),
                request_id,
                scalar(&scope.project_id)?,
                scalar(&scope.worktree_id)?,
            ),
        );
        Ok(())
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
        self.code("Coverage domains", list_or_none(&domains));
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
        self.text("Details", list_or_none(&details));
        let legal_actions = problem
            .legal_actions
            .iter()
            .map(scalar)
            .collect::<serde_json::Result<Vec<_>>>()?;
        self.code("Legal actions", list_or_none(&legal_actions));
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

    fn block(&mut self, label: &'static str, value: impl Into<String>) {
        self.fields.push(HumanField {
            label,
            value: HumanFieldValue::Block(value.into()),
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

fn list_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

const MAX_HUMAN_PAYLOAD_CHARS: usize = 6_000;

fn payload_preview(operation: &str, payload: Option<&Value>) -> serde_json::Result<String> {
    let Some(payload) = payload else {
        return Ok("none".to_owned());
    };
    let rendered = if operation == "source_body"
        && let Value::Object(fields) = payload
        && let (Some(file), Some(start_line), Some(end_line), Some(body)) = (
            fields.get("file").and_then(Value::as_str),
            fields.get("start_line").and_then(Value::as_u64),
            fields.get("end_line").and_then(Value::as_u64),
            fields.get("body").and_then(Value::as_str),
        ) {
        format!("{file}:{start_line}-{end_line}\n{body}")
    } else {
        match payload {
            Value::String(value) => value.clone(),
            _ => serde_json::to_string_pretty(payload)?,
        }
    };
    Ok(bounded_payload(rendered))
}

fn bounded_payload(rendered: String) -> String {
    let Some((end, _)) = rendered.char_indices().nth(MAX_HUMAN_PAYLOAD_CHARS) else {
        return rendered;
    };
    format!(
        "{}\n… payload truncated after {MAX_HUMAN_PAYLOAD_CHARS} characters; use --json for the complete typed result",
        &rendered[..end]
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_contracts::{
        CancellationObservation, CancellationStage, CoverageCompleteness, CoverageDomainState,
        Deadline, EvidenceCoverage, EvidenceDomain, OperationBudgetUsage, OperationReceipt,
        OperationTermination,
    };
    use tracedecay_domain::UtcMicros;

    use super::{CanonicalHumanView, HumanField, HumanFieldValue, payload_preview};
    use serde_json::Value;
    use tracedecay_contracts::{
        ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult, RequestId,
        ResultContractRef, SafeDiagnostic,
    };
    use tracedecay_tool_catalog::{BindingId, SchemaId};

    fn code(label: &'static str, value: &str) -> HumanField {
        HumanField {
            label,
            value: HumanFieldValue::Code(value.to_owned()),
        }
    }

    fn block(label: &'static str, value: &str) -> HumanField {
        HumanField {
            label,
            value: HumanFieldValue::Block(value.to_owned()),
        }
    }

    fn text(label: &'static str, value: &str) -> HumanField {
        HumanField {
            label,
            value: HumanFieldValue::Text(value.to_owned()),
        }
    }

    /// Partial evidence must be projected into the typed human fields that name
    /// what was covered — coverage, per-domain state, paging cursor, receipt,
    /// and cancellation. Markdown rendering and
    /// escaping belong to `cli::output::markdown`, so a display-label rename
    /// must not land here.
    #[test]
    fn partial_evidence_extracts_the_typed_coverage_fields() {
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
        view.block(
            "Payload",
            payload_preview("feedback_list", Some(&json!({"items": [1, 2]}))).unwrap(),
        );

        assert_eq!(view.heading, "feedback_list");
        assert_eq!(
            view.fields,
            vec![
                code("Coverage", "partial"),
                code("Coverage counts", "visited=5, eligible=4, returned=2"),
                code("Coverage domains", "source:partial, test:unknown"),
                code("Cursor", "cursor.opaque"),
                code("Termination", "partial"),
                code(
                    "Receipt",
                    "started=10, ended=20, deadline=30, units=3, bytes=40, elapsed_us=10",
                ),
                code("Cancellation stage", "during_read"),
                block("Payload", "{\n  \"items\": [\n    1,\n    2\n  ]\n}"),
            ]
        );
    }

    #[test]
    fn source_body_preview_leads_with_location_and_bounded_source() {
        let preview = payload_preview(
            "source_body",
            Some(&json!({
                "node_id": "symbol.example",
                "file": "src/lib.rs",
                "start_line": 7,
                "end_line": 9,
                "body": "pub fn answer() {\n    42\n}",
            })),
        )
        .unwrap();

        assert_eq!(preview, "src/lib.rs:7-9\npub fn answer() {\n    42\n}");
    }

    /// A canonical problem envelope must be projected into the full, ordered set of
    /// typed human fields — every field the CLI promises about a problem, in order,
    /// with the right `Code`/`Text` presentation. Markdown rendering and escaping
    /// are the contract of `cli::output::markdown`, not of this extraction, so a
    /// display-label rename must not land here.
    #[test]
    fn canonical_problem_view_extracts_the_typed_problem_fields() {
        let result: ApplicationResult<Value> = Err(ApplicationProblemEnvelope::new(
            ResultContractRef::new(SchemaId::new("schema.test.result").unwrap(), 3).unwrap(),
            RequestId::new("request.cli.golden").unwrap(),
            ApplicationProblem::unavailable(
                SafeDiagnostic::new(
                    "daemon_unavailable",
                    "The owning TraceDecay daemon is unavailable",
                )
                .unwrap(),
            ),
        )
        .expect("construct canonical CLI golden problem"));
        let view = CanonicalHumanView::from_application_result(
            "feedback_list",
            &BindingId::new("binding.cli.feedback-list.v1").unwrap(),
            &result,
        )
        .unwrap();

        assert_eq!(view.heading, "feedback_list");
        assert_eq!(
            view.fields,
            vec![
                code("Operation", "feedback_list"),
                code("Binding", "binding.cli.feedback-list.v1"),
                code("Status", "problem"),
                code("Contract", "schema.test.result@3"),
                code("Problem", "daemon_unavailable"),
                code("Problem kind", "unavailable"),
                code("Problem revision", "1"),
                code("Owning layer", "application"),
                code("Terminality", "pre_admission"),
                code("Request", "request.cli.golden"),
                code("Trace", "request.cli.golden"),
                text("Message", "The owning TraceDecay daemon is unavailable"),
                code("Retryable", "true"),
                code("Retry", "after_delay"),
                code("Retry scope", "same_request"),
                code("Retry after", "250ms"),
                code("Cancellation stage", "none"),
                text("Details", "none"),
                code("Legal actions", "retry"),
                code("Coverage", "not_available"),
            ]
        );
    }
}
