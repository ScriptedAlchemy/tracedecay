use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::time::Instant;
use tracedecay_application::{ApplicationEnvelope, ApplicationOutcome, ApplicationProblemEnvelope};

/// Parse a positive millisecond duration from `name`, falling back to
/// `default`. Values above `max` fail closed so CLI budgets cannot exceed
/// the supported monotonic range.
pub(crate) fn env_duration_ms(
    name: &str,
    default: Duration,
    max: Duration,
) -> tracedecay_domain::errors::Result<Duration> {
    let deadline = std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(default);
    if deadline > max {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("{name} exceeds the supported monotonic deadline range"),
        });
    }
    Ok(deadline)
}

/// Resolves the daemon handshake for the current client. One labeled
/// boundary so a slow CLI invocation can attribute time to client identity
/// resolution separately from the daemon round-trip itself.
#[hotpath::measure(label = "cli.daemon.handshake")]
fn client_handshake(
    project_path: Option<&std::path::Path>,
) -> tracedecay_domain::errors::Result<tracedecay_daemon_protocol::DaemonHandshake> {
    tracedecay::daemon::handshake_for_current_client(
        project_path.map(std::path::Path::to_path_buf),
        None,
        false,
        false,
    )
}

/// Typed result payload of a retained-surface daemon tool reply.
///
/// Retained tools answer with an [`ApplicationEnvelope`] whose evidence
/// packet carries the tool's result payload, or an
/// [`ApplicationProblemEnvelope`] carrying the typed refusal. Decoding the
/// raw reply directly into a `deny_unknown_fields` result type crashes on the
/// envelope's own fields (`contract`, `scope`, …), and indexing into the raw
/// reply misses the nested payload and reads as a silent empty. Every CLI
/// consumer of a retained result payload unwraps through here so a problem
/// envelope surfaces as its typed code and message and envelope drift
/// surfaces as a decode error naming the tool.
#[hotpath::measure(label = "cli.daemon.retained_payload")]
pub(crate) fn retained_tool_payload<T: DeserializeOwned>(
    tool_name: &str,
    reply: Value,
) -> tracedecay_domain::errors::Result<T> {
    let decode_error = |context: &str, error: serde_json::Error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned {context}: {error}"),
        }
    };
    if reply.get("problem").is_some() {
        let envelope: ApplicationProblemEnvelope = serde_json::from_value(reply)
            .map_err(|error| decode_error("an undecodable problem envelope", error))?;
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "daemon tool {tool_name} refused: {}: {}",
                envelope.problem.code, envelope.problem.message
            ),
        });
    }
    let envelope: ApplicationEnvelope<Value> = serde_json::from_value(reply)
        .map_err(|error| decode_error("an undecodable application envelope", error))?;
    let packet = match envelope.outcome {
        ApplicationOutcome::Evidence(packet) => packet,
        ApplicationOutcome::Preview(_) | ApplicationOutcome::Effect(_) => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("daemon tool {tool_name} returned a non-evidence outcome"),
            });
        }
    };
    let payload =
        packet
            .payload
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("daemon tool {tool_name} omitted its evidence payload"),
            })?;
    serde_json::from_value(payload).map_err(|error| decode_error("an undecodable payload", error))
}

/// One-shot daemon tool call using the shared `TRACEDECAY_TOOL_DEADLINE_MS`
/// envelope (default 120s) via `tracedecay::daemon::call_default_tool`.
pub(crate) async fn daemon_tool_json(
    project_path: Option<&std::path::Path>,
    tool_name: &str,
    arguments: serde_json::Value,
) -> tracedecay_domain::errors::Result<serde_json::Value> {
    #[cfg(feature = "hotpath")]
    hotpath::val!("cli.daemon.tool").set(&tool_name);
    let handshake = client_handshake(project_path)?;
    let result = hotpath::future!(
        tracedecay::daemon::call_default_tool(&handshake, tool_name, arguments),
        label = "cli.daemon.request"
    )
    .await?;
    recover_truncated_payload(&handshake, tool_name, result, None).await
}

/// Deadline-carrying variant for CLI journeys that deliberately trigger a cold
/// project open and wait it out (`tracedecay status` after a daemon restart).
/// The caller's wall-clock deadline bounds the open wait and the truncation
/// recovery fetch, so the command cannot outlive its own budget on private
/// retry clocks.
pub(crate) async fn daemon_tool_json_until(
    deadline: Instant,
    project_path: Option<&std::path::Path>,
    tool_name: &str,
    arguments: serde_json::Value,
) -> tracedecay_domain::errors::Result<serde_json::Value> {
    #[cfg(feature = "hotpath")]
    hotpath::val!("cli.daemon.tool").set(&tool_name);
    let handshake = client_handshake(project_path)?;
    // Distinct from `cli.daemon.request`: this lifetime includes waiting out a
    // cold project open, so aggregating the two would conflate daemon latency
    // with deliberate open waits.
    let result = hotpath::future!(
        tracedecay::daemon::call_default_tool_awaiting_project_open(
            &handshake, tool_name, arguments, deadline,
        ),
        label = "cli.daemon.request_open_wait"
    )
    .await?;
    recover_truncated_payload(&handshake, tool_name, result, Some(deadline)).await
}

async fn recover_truncated_payload(
    handshake: &tracedecay_daemon_protocol::DaemonHandshake,
    tool_name: &str,
    result: serde_json::Value,
    deadline: Option<Instant>,
) -> tracedecay_domain::errors::Result<serde_json::Value> {
    let payload = tracedecay::daemon::tool_json_payload(&result, tool_name)?;
    if !is_truncation_envelope(&payload) {
        return Ok(payload);
    }
    let handle = payload
        .get("handle")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "daemon tool {tool_name} returned truncated JSON without a retrieval handle"
            ),
        })?;
    let arguments = serde_json::json!({ "handle": handle, "format": "json" });
    let retrieved = match deadline {
        Some(deadline) => {
            hotpath::future!(
                tracedecay::daemon::call_default_tool_awaiting_project_open(
                    handshake,
                    "tracedecay_retrieve",
                    arguments,
                    deadline,
                ),
                label = "cli.daemon.recovery_fetch"
            )
            .await?
        }
        None => {
            hotpath::future!(
                tracedecay::daemon::call_default_tool(handshake, "tracedecay_retrieve", arguments),
                label = "cli.daemon.recovery_fetch"
            )
            .await?
        }
    };
    let retrieved = tracedecay::daemon::tool_json_payload(&retrieved, "tracedecay_retrieve")?;
    let content = retrieved
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("daemon retrieval for {tool_name} omitted response content"),
        })?;
    serde_json::from_str(content).map_err(Into::into)
}

/// Recover a truncated MCP tool result while keeping the MCP envelope shape
/// `tracedecay tool` prints. Status unwraps to the inner JSON; this path must
/// leave `content[*].text` as the recovered payload so `--format json` and
/// `--json` callers still parse the tool schema rather than a handle envelope.
pub(crate) async fn recover_truncated_mcp_result(
    handshake: &tracedecay_daemon_protocol::DaemonHandshake,
    tool_name: &str,
    result: serde_json::Value,
    deadline: Option<Instant>,
) -> tracedecay_domain::errors::Result<serde_json::Value> {
    let Ok(payload) = tracedecay::daemon::tool_json_payload(&result, tool_name) else {
        return Ok(result);
    };
    if !is_truncation_envelope(&payload) {
        return Ok(result);
    }
    let recovered =
        recover_truncated_payload(handshake, tool_name, result.clone(), deadline).await?;
    let text = serde_json::to_string(&recovered)?;
    let mut recovered_result = result;
    let blocks = recovered_result
        .get_mut("content")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} returned no content blocks"),
        })?;
    let mut replaced = false;
    for block in blocks {
        let Some(block_text) = block.get("text").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Ok(block_payload) = serde_json::from_str::<serde_json::Value>(block_text) else {
            continue;
        };
        if is_truncation_envelope(&block_payload) {
            block["text"] = serde_json::Value::String(text.clone());
            replaced = true;
            break;
        }
    }
    if !replaced {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("daemon tool {tool_name} omitted its truncation payload"),
        });
    }
    Ok(recovered_result)
}

pub(crate) fn is_truncation_envelope(value: &Value) -> bool {
    value.get("truncated").and_then(Value::as_bool) == Some(true)
        && value
            .get("original_chars")
            .and_then(Value::as_u64)
            .is_some()
        && value.get("preview").and_then(Value::as_str).is_some()
}

pub(crate) fn reject_truncation_envelope(
    value: &Value,
    tool_name: &str,
) -> tracedecay_domain::errors::Result<()> {
    if !is_truncation_envelope(value) {
        return Ok(());
    }
    let original_chars = value.get("original_chars").and_then(Value::as_u64);
    let handle = value.get("handle").and_then(Value::as_str);
    let message = match (original_chars, handle) {
        (Some(chars), Some(handle)) => format!(
            "daemon tool {tool_name} returned truncated JSON ({chars} chars); \
             recover with tracedecay_retrieve handle={handle}"
        ),
        (Some(chars), None) => format!(
            "daemon tool {tool_name} returned truncated JSON ({chars} chars) \
             without a retrieval handle"
        ),
        _ => format!("daemon tool {tool_name} returned truncated JSON"),
    };
    Err(tracedecay_domain::errors::TraceDecayError::Config { message })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;
    use tracedecay_application::{
        ApplicationProblem, ApplicationProblemEnvelope, AuthorityReceipt, CancellationContext,
        CapabilityGrantSnapshot, Deadline, DisclosureClass, EvidenceCoverage, EvidenceDomain,
        EvidencePacket, OperationReceipt, PageState, PolicyDecisionRef, RequestContext, RequestId,
        ResolvedScope, ResultContractRef, RetrievalEvidence, RetryDirective, TemporalState,
    };
    use tracedecay_domain::{
        ActorId, ComponentVersion, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, SchemaId, SortContractId, UseCaseId};

    use super::{is_truncation_envelope, retained_tool_payload};

    fn contract() -> ResultContractRef {
        ResultContractRef::new(SchemaId::new("schema.cli.fixture.result").unwrap(), 1).unwrap()
    }

    fn scope() -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new("project.cli.fixture").unwrap(),
            RepositoryId::new("repository.cli.fixture").unwrap(),
            WorktreeId::new("worktree.cli.fixture").unwrap(),
            None,
        )
        .unwrap()
    }

    fn digest(seed: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
    }

    fn context() -> RequestContext {
        let capability = CapabilityId::new("capability.cli.fixture").unwrap();
        let use_case = UseCaseId::new("use-case.cli.fixture").unwrap();
        let grant = CapabilityGrantSnapshot::new(
            "grant.cli.fixture".to_owned().try_into().unwrap(),
            1,
            digest('a'),
            ActorId::new("actor.issuer").unwrap(),
            UtcMicros(1),
            UtcMicros(1_000),
            scope(),
            BTreeSet::from([capability]),
            BTreeSet::from([use_case]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        RequestContext::new(
            ActorId::new("actor.requester").unwrap(),
            scope(),
            grant,
            RequestId::new("request.cli.fixture").unwrap(),
            Deadline::new(UtcMicros(500)).unwrap(),
            CancellationContext::active("cancel.cli.fixture").unwrap(),
        )
        .unwrap()
    }

    /// A daemon reply as retained tools actually send it: the typed payload
    /// nested inside a full evidence envelope.
    fn evidence_reply(payload: serde_json::Value) -> serde_json::Value {
        let context = context();
        let authority = AuthorityReceipt::from_context(
            &context,
            PolicyDecisionRef::new(
                "policy.cli.fixture",
                1,
                digest('b'),
                ComponentVersion::new("policy.evaluator.v1").unwrap(),
            )
            .unwrap(),
            UtcMicros(2),
        )
        .unwrap();
        let receipt = OperationReceipt::completed(
            UtcMicros(2),
            UtcMicros(3),
            context.deadline().clone(),
            Default::default(),
        )
        .unwrap();
        let evidence = RetrievalEvidence {
            payload: Some(payload),
            temporal: TemporalState::current(UtcMicros(2)),
            evidence_authorities: Vec::new(),
            coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Symbol], 1, 1, 1).unwrap(),
            omissions: Vec::new(),
            scores: Vec::new(),
            contributions: Vec::new(),
            page: PageState::first_page(
                SortContractId::new("sort.cli.fixture.v1").unwrap(),
                1,
                Some(1),
                1,
            )
            .unwrap(),
            finished_at: UtcMicros(3),
            budget: Default::default(),
            cancellation: None,
        };
        let packet = EvidencePacket::from_retrieval(evidence, authority, receipt).unwrap();
        serde_json::to_value(tracedecay_application::ApplicationEnvelope::evidence(
            contract(),
            context.request_id().clone(),
            scope(),
            packet,
        ))
        .unwrap()
    }

    /// The exact drift the memory-status defect shipped: a result type with
    /// `deny_unknown_fields` decoded straight from the envelope crashes on the
    /// envelope's `contract` field. The helper must reach the nested payload.
    #[test]
    fn evidence_envelope_unwraps_to_the_strict_result_payload() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct StrictResult {
            answer: i64,
        }

        let reply = evidence_reply(json!({ "answer": 42 }));
        assert!(
            serde_json::from_value::<StrictResult>(reply.clone()).is_err(),
            "the fixture must reproduce the raw-envelope decode crash"
        );

        let result: StrictResult =
            retained_tool_payload("tracedecay_memory_status", reply).expect("payload decodes");
        assert_eq!(result.answer, 42);
    }

    /// A problem envelope is a typed refusal, not a decode crash and not a
    /// silent empty.
    #[test]
    fn problem_envelope_surfaces_its_typed_code_and_message() {
        let reply = serde_json::to_value(
            ApplicationProblemEnvelope::new(
                contract(),
                RequestId::new("request.cli.fixture").unwrap(),
                ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
            )
            .unwrap(),
        )
        .unwrap();

        let error = retained_tool_payload::<serde_json::Value>("tracedecay_message_search", reply)
            .expect_err("a problem envelope is a refusal");
        let message = error.to_string();
        assert!(
            message.contains("tracedecay_message_search refused"),
            "refusal must name the tool: {message}"
        );
        assert!(
            message.contains("not_found"),
            "refusal must carry the typed problem code: {message}"
        );
    }

    /// Envelope drift fails with an error naming the tool instead of a bare
    /// serde message.
    #[test]
    fn non_envelope_reply_is_a_named_decode_error() {
        let error = retained_tool_payload::<serde_json::Value>(
            "tracedecay_memory_status",
            json!({ "memory": {} }),
        )
        .expect_err("a bare payload is envelope drift");
        assert!(
            error
                .to_string()
                .contains("tracedecay_memory_status returned an undecodable application envelope"),
            "decode error must name the tool: {error}"
        );
    }

    #[test]
    fn bounded_tool_coverage_is_not_a_transport_truncation_envelope() {
        let grep_payload = json!({
            "results": [],
            "match_count": 0,
            "truncated": true,
            "coverage": { "completeness": "partial" },
        });

        assert!(!is_truncation_envelope(&grep_payload));
        assert!(is_truncation_envelope(&json!({
            "truncated": true,
            "original_chars": 20_000,
            "preview_chars": 10_000,
            "preview": "prefix",
            "handle": "rh_0123456789abcdef01234567",
        })));
    }
}
