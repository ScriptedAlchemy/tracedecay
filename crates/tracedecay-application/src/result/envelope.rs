use serde::{Deserialize, Deserializer, Serialize, de::MapAccess, de::SeqAccess, de::Visitor};
use std::fmt;
use tracedecay_tool_catalog::{SchemaId, SchemaRef};

use crate::context::{RequestId, ResolvedScope};
use crate::error::ApplicationContractError;

use super::{
    ApplicationProblem, ApplicationProblemKind, CancellationStage, EffectReceipt, EffectResult,
    EvidenceCoverage, EvidencePacket, LegalAction, PreviewResult, ProblemOwningLayer,
    ProblemTerminality, RetryDirective, RetryScope, SafeDiagnostic,
};

pub const APPLICATION_PROBLEM_REVISION: u32 = 1;
pub const MAX_PROBLEM_DETAILS: usize = 8;
pub const MAX_RETRY_AFTER_MILLIS: u64 = 24 * 60 * 60 * 1_000;

/// Canonical delay stamped whenever a problem carries
/// `RetryDirective::AfterDelay` and its producer supplied no explicit figure.
/// Matches the saturation backoff the operation-event stream already uses.
pub const DEFAULT_RETRY_AFTER_MILLIS: u64 = 250;

/// Versioned schema identity for an application result contract.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ResultContractRef {
    schema_id: SchemaId,
    schema_revision: u32,
}

impl<'de> Deserialize<'de> for ResultContractRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            schema_id: SchemaId,
            schema_revision: u32,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.schema_id, wire.schema_revision).map_err(serde::de::Error::custom)
    }
}

impl ResultContractRef {
    pub fn new(
        schema_id: SchemaId,
        schema_revision: u32,
    ) -> Result<Self, ApplicationContractError> {
        if schema_revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "result schema revision",
            });
        }
        Ok(Self {
            schema_id,
            schema_revision,
        })
    }

    pub fn from_schema(schema: &SchemaRef) -> Self {
        Self {
            schema_id: schema.schema_id().clone(),
            schema_revision: schema.revision(),
        }
    }

    pub fn schema_id(&self) -> &SchemaId {
        &self.schema_id
    }

    pub const fn schema_revision(&self) -> u32 {
        self.schema_revision
    }
}

/// Canonical outcome family for an admitted application operation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "value")]
pub enum ApplicationOutcome<T> {
    Evidence(EvidencePacket<T>),
    Preview(PreviewResult<T>),
    Effect(EffectResult<T>),
}

/// Successful application result with a stable contract, request, and scope.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationEnvelope<T> {
    pub contract: ResultContractRef,
    pub request_id: RequestId,
    pub scope: ResolvedScope,
    pub outcome: ApplicationOutcome<T>,
}

impl<T> ApplicationEnvelope<T> {
    pub fn evidence(
        contract: ResultContractRef,
        request_id: RequestId,
        scope: ResolvedScope,
        packet: EvidencePacket<T>,
    ) -> Self {
        Self {
            contract,
            request_id,
            scope,
            outcome: ApplicationOutcome::Evidence(packet),
        }
    }

    pub fn preview(
        contract: ResultContractRef,
        request_id: RequestId,
        scope: ResolvedScope,
        preview: PreviewResult<T>,
    ) -> Self {
        Self {
            contract,
            request_id,
            scope,
            outcome: ApplicationOutcome::Preview(preview),
        }
    }

    pub fn effect(
        contract: ResultContractRef,
        request_id: RequestId,
        scope: ResolvedScope,
        effect: EffectResult<T>,
    ) -> Self {
        Self {
            contract,
            request_id,
            scope,
            outcome: ApplicationOutcome::Effect(effect),
        }
    }
}

/// Stable application problem record shared verbatim by every adapter.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationProblemRecord {
    pub revision: u32,
    pub kind: ApplicationProblemKind,
    pub code: String,
    pub message: String,
    pub diagnostic: Option<SafeDiagnostic>,
    /// A committed effect is present only for an admitted partial effect.
    /// The nullable field is always serialized: omitting it would create a
    /// compatibility/default path that could hide a missing receipt.
    pub committed_receipt: Option<EffectReceipt>,
    pub owning_layer: ProblemOwningLayer,
    pub terminality: ProblemTerminality,
    pub retryable: bool,
    pub retry: RetryDirective,
    pub retry_scope: Option<RetryScope>,
    pub retry_after_millis: Option<u64>,
    pub cancellation_stage: Option<CancellationStage>,
    pub request_id: RequestId,
    pub trace_id: RequestId,
    pub details: Vec<SafeDiagnostic>,
    pub legal_actions: Vec<LegalAction>,
    pub coverage: Option<EvidenceCoverage>,
    #[serde(skip)]
    source: ApplicationProblem,
}

impl<'de> Deserialize<'de> for ApplicationProblemRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            revision: u32,
            kind: ApplicationProblemKind,
            code: String,
            message: String,
            diagnostic: Option<SafeDiagnostic>,
            committed_receipt: RequiredNullable<EffectReceipt>,
            owning_layer: ProblemOwningLayer,
            terminality: ProblemTerminality,
            retryable: bool,
            retry: RetryDirective,
            retry_scope: Option<RetryScope>,
            retry_after_millis: Option<u64>,
            cancellation_stage: Option<CancellationStage>,
            request_id: RequestId,
            trace_id: RequestId,
            details: Vec<SafeDiagnostic>,
            legal_actions: Vec<LegalAction>,
            coverage: Option<EvidenceCoverage>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let source = match (
            wire.kind,
            wire.diagnostic.clone(),
            wire.committed_receipt.0.clone(),
        ) {
            (ApplicationProblemKind::InvalidRequest, Some(diagnostic), None) => {
                ApplicationProblem::InvalidRequest {
                    diagnostic,
                    retry: wire.retry,
                    legal_actions: wire.legal_actions.clone(),
                }
            }
            (ApplicationProblemKind::NotFoundOrNotAuthorized, None, None) => {
                ApplicationProblem::NotFoundOrNotAuthorized {
                    retry: wire.retry,
                    legal_actions: wire.legal_actions.clone(),
                }
            }
            (ApplicationProblemKind::Conflict, Some(diagnostic), None) => {
                ApplicationProblem::Conflict {
                    diagnostic,
                    retry: wire.retry,
                    legal_actions: wire.legal_actions.clone(),
                }
            }
            (ApplicationProblemKind::PartialEffect, Some(diagnostic), Some(committed_receipt)) => {
                ApplicationProblem::PartialEffect {
                    diagnostic,
                    committed_receipt,
                    retry: wire.retry,
                    legal_actions: wire.legal_actions.clone(),
                }
            }
            (ApplicationProblemKind::Stale, Some(diagnostic), None) => ApplicationProblem::Stale {
                diagnostic,
                retry: wire.retry,
                legal_actions: wire.legal_actions.clone(),
            },
            (ApplicationProblemKind::Unsupported, Some(diagnostic), None) => {
                ApplicationProblem::Unsupported {
                    diagnostic,
                    retry: wire.retry,
                    legal_actions: wire.legal_actions.clone(),
                }
            }
            (ApplicationProblemKind::Unavailable, Some(diagnostic), None) => {
                ApplicationProblem::Unavailable {
                    diagnostic,
                    retry: wire.retry,
                    legal_actions: wire.legal_actions.clone(),
                }
            }
            (ApplicationProblemKind::ResetRequired, Some(diagnostic), None) => {
                ApplicationProblem::ResetRequired {
                    diagnostic,
                    retry: wire.retry,
                    legal_actions: wire.legal_actions.clone(),
                }
            }
            (ApplicationProblemKind::Saturated, Some(diagnostic), None) => {
                ApplicationProblem::Saturated {
                    diagnostic,
                    retry: wire.retry,
                    legal_actions: wire.legal_actions.clone(),
                }
            }
            (ApplicationProblemKind::Cancelled, None, None) => ApplicationProblem::Cancelled {
                retry: wire.retry,
                legal_actions: wire.legal_actions.clone(),
            },
            (ApplicationProblemKind::TimedOut, None, None) => ApplicationProblem::TimedOut {
                retry: wire.retry,
                legal_actions: wire.legal_actions.clone(),
            },
            _ => {
                return Err(serde::de::Error::custom(
                    "invalid application problem shape",
                ));
            }
        };
        let canonical =
            Self::new(wire.request_id.clone(), source).map_err(serde::de::Error::custom)?;
        let record = Self {
            revision: wire.revision,
            kind: wire.kind,
            code: wire.code,
            message: wire.message,
            diagnostic: wire.diagnostic,
            committed_receipt: wire.committed_receipt.0,
            owning_layer: wire.owning_layer,
            terminality: wire.terminality,
            retryable: wire.retryable,
            retry: wire.retry,
            retry_scope: wire.retry_scope,
            retry_after_millis: wire.retry_after_millis,
            cancellation_stage: wire.cancellation_stage,
            request_id: wire.request_id,
            trace_id: wire.trace_id,
            details: wire.details,
            legal_actions: wire.legal_actions,
            coverage: wire.coverage,
            source: canonical.source,
        };
        record.validate().map_err(serde::de::Error::custom)?;
        Ok(record)
    }
}

/// Unlike `Option<T>`, this wrapper distinguishes an explicit JSON `null`
/// from an omitted field. New terminal-state fields must be present on every
/// record so a missing committed receipt cannot be mistaken for `None`.
struct RequiredNullable<T>(Option<T>);

impl<'de, T> Deserialize<'de> for RequiredNullable<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RequiredNullableVisitor<T>(std::marker::PhantomData<fn() -> T>);

        impl<'de, T> Visitor<'de> for RequiredNullableVisitor<T>
        where
            T: Deserialize<'de>,
        {
            type Value = RequiredNullable<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a nullable value whose field is present")
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(RequiredNullable(None))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(RequiredNullable(None))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(Some)
                    .map(RequiredNullable)
            }

            fn visit_seq<A>(self, sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                T::deserialize(serde::de::value::SeqAccessDeserializer::new(sequence))
                    .map(Some)
                    .map(RequiredNullable)
            }
        }

        deserializer.deserialize_any(RequiredNullableVisitor(std::marker::PhantomData))
    }
}

impl ApplicationProblemRecord {
    fn new(
        request_id: RequestId,
        source: ApplicationProblem,
    ) -> Result<Self, ApplicationContractError> {
        source.validate()?;
        let retry = source.retry();
        let kind = source.kind();
        let retry_scope = match retry {
            RetryDirective::Never => None,
            RetryDirective::SameRequest | RetryDirective::AfterDelay => {
                Some(RetryScope::SameRequest)
            }
            RetryDirective::AfterRevalidate => Some(RetryScope::FreshRequest),
            RetryDirective::AfterReconcile => Some(RetryScope::SameOperation),
        };
        let diagnostic = source.diagnostic().cloned();
        let committed_receipt = source.committed_receipt().cloned();
        let code = diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code.clone())
            .unwrap_or_else(|| source.canonical_code().to_owned());
        let record = Self {
            revision: APPLICATION_PROBLEM_REVISION,
            kind,
            code,
            message: source.safe_message().to_owned(),
            diagnostic,
            committed_receipt,
            owning_layer: ProblemOwningLayer::Application,
            terminality: kind.terminality(),
            retryable: retry != RetryDirective::Never,
            retry,
            retry_scope,
            // An `after_delay` directive promises a delay: the serialized
            // contract (enforced by every generated SDK client) rejects the
            // directive with a null delay, so the canonical default fills it
            // here at the single construction authority. Callers that know a
            // better figure override via `with_retry_after_millis`.
            retry_after_millis: (retry == RetryDirective::AfterDelay)
                .then_some(DEFAULT_RETRY_AFTER_MILLIS),
            cancellation_stage: matches!(
                kind,
                ApplicationProblemKind::Cancelled | ApplicationProblemKind::TimedOut
            )
            .then_some(CancellationStage::BeforeAdmission),
            trace_id: request_id.clone(),
            request_id,
            details: Vec::new(),
            legal_actions: source.legal_actions().to_vec(),
            coverage: None,
            source,
        };
        record.validate()?;
        Ok(record)
    }

    /// Validate the wire-visible record against its source problem.  This is
    /// intentionally strict: a record must never lose a committed receipt or
    /// turn an admitted terminal into an unavailable pre-admission failure.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.source.validate()?;
        if self.revision != APPLICATION_PROBLEM_REVISION
            || self.kind != self.source.kind()
            || self.terminality != self.kind.terminality()
            || self.retry != self.source.retry()
            || self.retryable != (self.retry != RetryDirective::Never)
            || self.legal_actions != self.source.legal_actions()
            || self.request_id != self.trace_id
            || self.details.len() > MAX_PROBLEM_DETAILS
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "application problem record",
            });
        }

        if self.code
            != self
                .source
                .diagnostic()
                .map(|diagnostic| diagnostic.code.as_str())
                .unwrap_or_else(|| self.source.canonical_code())
            || self.message != self.source.safe_message()
            || self.diagnostic.as_ref() != self.source.diagnostic()
            || self.committed_receipt.as_ref() != self.source.committed_receipt()
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "application problem identity",
            });
        }

        let expected_retry_scope = match self.retry {
            RetryDirective::Never => None,
            RetryDirective::SameRequest | RetryDirective::AfterDelay => {
                Some(RetryScope::SameRequest)
            }
            RetryDirective::AfterRevalidate => Some(RetryScope::FreshRequest),
            RetryDirective::AfterReconcile => Some(RetryScope::SameOperation),
        };
        if self.retry_scope != expected_retry_scope
            || self
                .retry_after_millis
                .is_some_and(|delay| delay > MAX_RETRY_AFTER_MILLIS)
            || (self.retry_after_millis.is_some() && !self.retryable)
            || (self.retry == RetryDirective::AfterDelay && self.retry_after_millis.is_none())
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "problem retry delay",
            });
        }

        let expected_cancellation_stage = matches!(
            self.kind,
            ApplicationProblemKind::Cancelled | ApplicationProblemKind::TimedOut
        )
        .then_some(CancellationStage::BeforeAdmission);
        if self.cancellation_stage != expected_cancellation_stage {
            return Err(ApplicationContractError::Inconsistent {
                field: "problem cancellation stage",
            });
        }

        if matches!(
            self.kind,
            ApplicationProblemKind::PartialEffect | ApplicationProblemKind::ResetRequired
        ) && self.retry != RetryDirective::Never
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "admitted terminal retry",
            });
        }

        if let Some(diagnostic) = &self.diagnostic {
            diagnostic.validate()?;
        }
        for detail in &self.details {
            detail.validate()?;
        }
        if let Some(receipt) = &self.committed_receipt {
            receipt.validate()?;
            if receipt.request_id != self.request_id {
                return Err(ApplicationContractError::Inconsistent {
                    field: "committed receipt request identity",
                });
            }
        }
        if let Some(coverage) = &self.coverage {
            coverage.validate()?;
        }
        Ok(())
    }

    pub fn kind(&self) -> ApplicationProblemKind {
        self.kind
    }

    pub fn is_pre_admission(&self) -> bool {
        self.terminality == ProblemTerminality::PreAdmission
    }

    pub fn is_admitted_terminal(&self) -> bool {
        self.terminality == ProblemTerminality::AdmittedTerminal
    }

    pub fn source(&self) -> &ApplicationProblem {
        &self.source
    }

    pub fn into_source(self) -> ApplicationProblem {
        self.source
    }
}

/// Stable application failure envelope. Partial effects and reset-required
/// states are admitted terminals; partial effects carry their committed
/// receipt directly while reset-required states carry an explicit action.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationProblemEnvelope {
    pub contract: ResultContractRef,
    pub request_id: RequestId,
    // Boxed: the problem record dominates the envelope's size, and this
    // envelope is the Err variant of every application result.
    pub problem: Box<ApplicationProblemRecord>,
}

impl<'de> Deserialize<'de> for ApplicationProblemEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            contract: ResultContractRef,
            request_id: RequestId,
            problem: Box<ApplicationProblemRecord>,
        }

        let wire = Wire::deserialize(deserializer)?;
        if wire.request_id != wire.problem.request_id {
            return Err(serde::de::Error::custom(
                "application envelope request identity does not match its problem record",
            ));
        }
        if wire.contract.schema_revision == 0 {
            return Err(serde::de::Error::custom(
                "application result schema revision must be greater than zero",
            ));
        }
        Ok(Self {
            contract: wire.contract,
            request_id: wire.request_id,
            problem: wire.problem,
        })
    }
}

impl ApplicationProblemEnvelope {
    pub fn new(
        contract: ResultContractRef,
        request_id: RequestId,
        problem: ApplicationProblem,
    ) -> Result<Self, ApplicationContractError> {
        let record = ApplicationProblemRecord::new(request_id.clone(), problem)?;
        Ok(Self {
            contract,
            request_id,
            problem: Box::new(record),
        })
    }

    pub fn with_owning_layer(mut self, owning_layer: ProblemOwningLayer) -> Self {
        self.problem.owning_layer = owning_layer;
        self
    }

    pub fn with_retry_after_millis(
        mut self,
        retry_after_millis: Option<u64>,
    ) -> Result<Self, ApplicationContractError> {
        if retry_after_millis.is_some_and(|delay| delay > MAX_RETRY_AFTER_MILLIS)
            || (retry_after_millis.is_some() && !self.problem.retryable)
            || (self.problem.retry == RetryDirective::AfterDelay && retry_after_millis.is_none())
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "problem retry delay",
            });
        }
        self.problem.retry_after_millis = retry_after_millis;
        Ok(self)
    }

    pub fn with_coverage(
        mut self,
        coverage: EvidenceCoverage,
    ) -> Result<Self, ApplicationContractError> {
        coverage.validate()?;
        self.problem.coverage = Some(coverage);
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectTermination, IdempotencyKey};
    use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, RepositoryId, WorktreeId};
    use tracedecay_tool_catalog::{EffectClass, SchemaId, UseCaseId};

    fn digest(seed: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64)))
            .expect("fixture digest is valid")
    }

    fn receipt() -> EffectReceipt {
        let expected_state = digest('a');
        EffectReceipt {
            operation: UseCaseId::new("use-case.result.fixture").expect("fixture use case"),
            request_id: RequestId::new("request.result.fixture").expect("fixture request"),
            actor: ActorId::new("actor.result.fixture").expect("fixture actor"),
            scope: ResolvedScope::new(
                ProjectId::new("project.result.fixture").expect("fixture project"),
                RepositoryId::new("repository.result.fixture").expect("fixture repository"),
                WorktreeId::new("worktree.result.fixture").expect("fixture worktree"),
                None,
            )
            .expect("fixture scope"),
            effect_class: EffectClass::Administrative,
            idempotency_key: IdempotencyKey::new("idempotency.result.fixture")
                .expect("fixture idempotency key"),
            input_digest: digest('a'),
            expected_state,
            policy_digest: digest('b'),
            configuration_digest: digest('c'),
            catalog_digest: digest('d'),
            privacy_digest: digest('e'),
            outcome: EffectTermination::Partial,
            committed_state: Some(digest('f')),
            external_proof: None,
        }
    }

    fn contract() -> ResultContractRef {
        ResultContractRef::new(SchemaId::new("schema.result.fixture").expect("schema"), 1)
            .expect("result contract")
    }

    #[test]
    fn partial_effect_record_round_trips_its_receipt_and_terminality() {
        let envelope = ApplicationProblemEnvelope::new(
            contract(),
            RequestId::new("request.result.fixture").expect("request"),
            ApplicationProblem::PartialEffect {
                diagnostic: SafeDiagnostic::new(
                    "result.partial_effect",
                    "The effect committed but delivery did not complete.",
                )
                .expect("diagnostic"),
                committed_receipt: receipt(),
                retry: RetryDirective::Never,
                legal_actions: vec![LegalAction::Reconcile],
            },
        )
        .expect("partial-effect envelope is valid")
        .with_owning_layer(ProblemOwningLayer::Port);

        assert!(envelope.problem.is_admitted_terminal());
        envelope.problem.validate().expect("record is canonical");
        let wire = serde_json::to_value(&envelope).expect("envelope serializes");
        assert_eq!(wire["problem"]["kind"], "partial_effect");
        assert_eq!(
            wire["problem"]["terminality"],
            serde_json::json!("admitted_terminal")
        );
        assert!(wire["problem"]["committed_receipt"].is_object());

        let decoded: ApplicationProblemEnvelope =
            serde_json::from_value(wire.clone()).expect("canonical envelope decodes");
        assert_eq!(decoded, envelope);

        let mut failed_receipt =
            serde_json::to_value(envelope.problem.source()).expect("standalone problem serializes");
        failed_receipt["committed_receipt"]["outcome"] = serde_json::json!("failed");
        assert!(serde_json::from_value::<ApplicationProblem>(failed_receipt).is_err());

        let mut empty_commit =
            serde_json::to_value(envelope.problem.source()).expect("standalone problem serializes");
        empty_commit["committed_receipt"]["committed_state"] = serde_json::Value::Null;
        empty_commit["committed_receipt"]["external_proof"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<ApplicationProblem>(empty_commit).is_err());

        let mut mismatched_request = wire.clone();
        mismatched_request["request_id"] = serde_json::json!("request.other.fixture");
        assert!(serde_json::from_value::<ApplicationProblemEnvelope>(mismatched_request).is_err());

        let mut missing_receipt = wire.clone();
        missing_receipt["problem"]
            .as_object_mut()
            .expect("problem object")
            .remove("committed_receipt");
        assert!(serde_json::from_value::<ApplicationProblemEnvelope>(missing_receipt).is_err());

        let mut downgraded = wire;
        downgraded["problem"]["kind"] = serde_json::json!("unavailable");
        assert!(serde_json::from_value::<ApplicationProblemEnvelope>(downgraded).is_err());
    }

    #[test]
    fn reset_required_record_round_trips_without_a_compatibility_default() {
        let envelope = ApplicationProblemEnvelope::new(
            contract(),
            RequestId::new("request.reset.fixture").expect("request"),
            ApplicationProblem::reset_required(
                SafeDiagnostic::new(
                    "result.reset_required",
                    "The store requires an explicit reset.",
                )
                .expect("diagnostic"),
            ),
        )
        .expect("reset-required envelope is valid");
        let wire = serde_json::to_value(&envelope).expect("envelope serializes");
        assert_eq!(wire["problem"]["kind"], "reset_required");
        assert_eq!(
            wire["problem"]["terminality"],
            serde_json::json!("admitted_terminal")
        );
        assert_eq!(
            wire["problem"]["committed_receipt"],
            serde_json::Value::Null
        );
        assert_eq!(
            wire["problem"]["legal_actions"],
            serde_json::json!(["reset"])
        );

        let decoded: ApplicationProblemEnvelope =
            serde_json::from_value(wire.clone()).expect("canonical envelope decodes");
        assert_eq!(decoded, envelope);

        let mut unknown_contract = wire.clone();
        unknown_contract["contract"]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ApplicationProblemEnvelope>(unknown_contract).is_err());

        let mut missing_receipt = wire;
        missing_receipt["problem"]
            .as_object_mut()
            .expect("problem object")
            .remove("committed_receipt");
        assert!(serde_json::from_value::<ApplicationProblemEnvelope>(missing_receipt).is_err());
    }
}

pub type ApplicationResult<T> = Result<ApplicationEnvelope<T>, ApplicationProblemEnvelope>;
