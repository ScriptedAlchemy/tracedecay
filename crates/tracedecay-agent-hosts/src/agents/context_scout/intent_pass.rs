//! The Context Scout session intent pass.
//!
//! One admitted prompt, tool, or stop boundary reads a bounded window of the
//! session's own ingested transcript ([`super::session_window`]), asks the
//! configured decision engine one batched set of yes/no intent questions
//! ([`super::decision_engine`]), derives deterministic action facts from the
//! window's newest turn, and turns the policy decision into at most one
//! session-evidence suggestion at the session's intent-scoped address. The
//! window body is transient: only its digest and store-id bounds enter the
//! durable claim, and the engine receives it once.
//!
//! Every early return is a typed outcome. A model never chooses receptivity:
//! quiet mode, spacing, duplicates, and the one-pending-per-session rule are
//! decided here and in the owner, after the engine answered.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracedecay_contracts::context_scout::{
    ContextScoutCandidateV1, ContextScoutCategoryV1, ContextScoutDeliveryReceiptV1,
    ContextScoutDeliveryWindowV1, ContextScoutDurableQueueEntryV1,
    ContextScoutEvidenceAvailabilityV1, ContextScoutIntentAnswerV1, ContextScoutIntentReceiptV1,
    ContextScoutModelOutcomeV1, ContextScoutRedactionReceiptV1, ContextScoutSessionEvidenceV1,
    ContextScoutSessionWindowV1, DecisionEngineIdentityV1 as ReceiptEngineIdentityV1,
};
use tracedecay_contracts::{AuthorityReceipt, PolicyDecisionRef, RequestContext};
use tracedecay_domain::configuration::{
    ContextScoutIntentKindV1, ContextScoutIntentSettingsV1, INTENT_MAX_PERMILLE,
};
use tracedecay_domain::{
    CodeGenerationId, ComponentVersion, ManifestDigest, NativeHostIdentityV1, ProviderId,
    UtcMicros, canonical_sha256,
};
use tracedecay_global_db::{AnalyticsEventInsert, RegisteredGlobalDb};
use tracedecay_policy::intent_action_facts::{ActionFactInputV1, action_facts};
use tracedecay_policy::intent_catalog::suggestion_text;
use tracedecay_policy::intent_decision::{
    IntentCandidateV1, IntentDecisionInputV1, IntentDecisionV1, decide_intents,
};
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};
use tracedecay_runtime_core::native_runtime;
use tracedecay_sessions::SessionProvider;

use super::address_registry::{
    AdmittedContextScoutHookV1, ContextScoutAddressBindOutcomeV1, ContextScoutAuthorityPinV1,
    ContextScoutLifecycleAddressV1, ProjectContextScoutAddressRegistryV1,
};
use super::decision_engine::{
    DecisionAnswerV1, DecisionEngineOutcomeV1, DecisionEnginePortV1, DecisionQuestionKindV1,
    DecisionQuestionV1, DecisionRequestV1,
};
use super::evidence::MAX_DECISION_ENGINE_MODEL_BYTES;
use super::owner::{ContextScoutIntentSessionStateV1, ProjectContextScoutOwnerV1};
use super::session_window::{
    SessionWindowFreshnessV1, SessionWindowLimitsV1, SessionWindowOutcomeV1,
    SessionWindowRequestV1, SessionWindowRoleV1, SessionWindowV1, read_session_window,
    render_window_state,
};
use super::{
    ContextScoutDeliverySelectionInputV1, ContextScoutDurableStoreOutcomeV1,
    ContextScoutRuntimeOutcomeV1, ContextScoutSelectionInputV1, ContextScoutServiceStateV1,
    ContextScoutSessionClaimInputV1, ContextScoutSessionEvidenceExt, ContextScoutSuppressionV1,
    ContextScoutTriggerV1, select_context_scout_delivery_window,
};

const INTENT_CANDIDATE_DEDUPE_DOMAIN: &str = "tracedecay.context-scout.intent-candidate.v1";
const INTENT_ENVELOPE_DOMAIN: &str = "tracedecay.context-scout.intent-envelope.v1";
const INTENT_WATERMARK_DOMAIN: &str = "tracedecay.context-scout.intent-watermark.v1";
const INTENT_POLICY_ID: &str = "policy.context-scout.intent-pass.v1";
const INTENT_POLICY_VERSION: &str = "tracedecay.context-scout.intent-pass.v1";
/// Interval between window reads while the pass waits for the boundary's
/// turn to be ingested.
const FRESHEN_POLL: Duration = Duration::from_millis(50);
/// Hosts record transcript times in whole seconds or milliseconds, so a turn
/// written in the same second as the boundary may read up to a second early.
const BOUNDARY_TIMESTAMP_SLACK_MICROS: i64 = 1_000_000;

/// Where the pass records the suggestions it withheld, beside the hook hint
/// analytics hint settlement reads.
pub struct ContextScoutIntentAnalyticsV1<'a> {
    pub analytics: &'a RegisteredGlobalDb,
    /// `RegisteredGlobalDb::canonical_project_key` of the project root.
    pub project_key: &'a str,
}

/// Every input of one intent pass. The caller has admitted the hook, pinned
/// configuration and scope into `pin`, and authorized `context`.
pub struct ContextScoutIntentPassV1<'a> {
    pub owner: &'a ProjectContextScoutOwnerV1,
    pub registry: &'a ProjectContextScoutAddressRegistryV1,
    /// The project session store the hosts ingest transcripts into.
    pub sessions: &'a RegisteredGlobalDb,
    pub analytics: Option<ContextScoutIntentAnalyticsV1<'a>>,
    pub hook: &'a AdmittedContextScoutHookV1,
    pub pin: &'a ContextScoutAuthorityPinV1,
    pub context: &'a RequestContext,
    /// The session's native lifecycle; the pass binds its intent scope.
    pub lifecycle: &'a ContextScoutLifecycleAddressV1,
    /// The project's current ready code generation. Session evidence is
    /// pinned to it, so a project without one refuses the pass.
    pub code_generation_id: Option<CodeGenerationId>,
    /// The engine of the installed configuration; `None` when the configured
    /// engine was refused.
    pub engine: Option<Arc<dyn DecisionEnginePortV1>>,
    pub observed_at: UtcMicros,
    pub deadline: MonotonicDeadline,
    pub cancellation: CancellationToken,
}

/// Typed end of one intent pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextScoutIntentPassOutcomeV1 {
    /// No enabled intent pass is configured; nothing was read or asked.
    Disabled,
    /// The project has no ready code generation to pin evidence to.
    CodeGenerationUnavailable,
    /// The host has no ingested transcript provider.
    ProviderUnsupported,
    /// The session's intent-scoped address could not be bound.
    AddressUnavailable,
    WindowUnavailable,
    WindowEmpty,
    WindowStale,
    Cancelled,
    /// The engine and action facts named nothing to suggest.
    NoCandidate {
        model_outcome: ContextScoutModelOutcomeV1,
    },
    Suppressed {
        reason: ContextScoutSuppressionV1,
        model_outcome: ContextScoutModelOutcomeV1,
    },
    Enqueued {
        entry: Box<ContextScoutDurableQueueEntryV1>,
        /// The intent-scoped lifecycle the entry's address is bound to.
        lifecycle: Box<ContextScoutLifecycleAddressV1>,
    },
    Unavailable,
}

impl ContextScoutIntentPassOutcomeV1 {
    /// Stable operator-log name of the outcome.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::CodeGenerationUnavailable => "code_generation_unavailable",
            Self::ProviderUnsupported => "provider_unsupported",
            Self::AddressUnavailable => "address_unavailable",
            Self::WindowUnavailable => "window_unavailable",
            Self::WindowEmpty => "window_empty",
            Self::WindowStale => "window_stale",
            Self::Cancelled => "cancelled",
            Self::NoCandidate { .. } => "no_candidate",
            Self::Suppressed { .. } => "suppressed",
            Self::Enqueued { .. } => "enqueued",
            Self::Unavailable => "unavailable",
        }
    }
}

/// The LCM transcript provider a hook host ingests under. Cursor's desktop
/// and cloud surfaces share one transcript provider; Droid ingests none.
pub fn intent_session_provider(host: NativeHostIdentityV1) -> Option<SessionProvider> {
    match host {
        NativeHostIdentityV1::ClaudeCode => Some(SessionProvider::Claude),
        NativeHostIdentityV1::CursorDesktop | NativeHostIdentityV1::CursorCloud => {
            Some(SessionProvider::Cursor)
        }
        NativeHostIdentityV1::Codex => Some(SessionProvider::Codex),
        NativeHostIdentityV1::Hermes => Some(SessionProvider::Hermes),
        NativeHostIdentityV1::Kiro => Some(SessionProvider::Kiro),
        NativeHostIdentityV1::Cline => Some(SessionProvider::Cline),
        NativeHostIdentityV1::RooCode => Some(SessionProvider::RooCode),
        NativeHostIdentityV1::Kilo => Some(SessionProvider::Kilo),
        NativeHostIdentityV1::KimiCode => Some(SessionProvider::Kimi),
        NativeHostIdentityV1::OpenCode => Some(SessionProvider::OpenCode),
        NativeHostIdentityV1::Pi => Some(SessionProvider::Pi),
        NativeHostIdentityV1::FactoryDroid => None,
    }
}

/// Analytics provider of an intent suggestion: hint settlement strips the
/// `hook_` prefix to find the session's ingested activity.
pub fn intent_analytics_provider(provider_id: &ProviderId) -> String {
    format!("hook_{}", provider_id.as_str())
}

/// The `hint_emitted` analytics row of one delivered intent-pass suggestion.
/// Hint settlement resolves it into `hint_outcome` acted/ignored from the
/// session's later ingested tool activity, exactly as for hook hints; the
/// metadata carries the decision receipt so the dashboard and fine-tuning
/// exports can explain it without the window body. `None` for code evidence.
pub fn intent_hint_emitted_event(
    project_key: &str,
    entry: &ContextScoutDurableQueueEntryV1,
    receipt: &ContextScoutDeliveryReceiptV1,
) -> Option<AnalyticsEventInsert> {
    let evidence = entry.envelope.candidate.evidence.as_session()?;
    let decision = &evidence.decision;
    let metadata = serde_json::json!({
        "source": "context_scout_intent_pass",
        "route": entry.route,
        "engine": decision.engine,
        "model_outcome": decision.outcome,
        "decision_source": decision.source,
        "answers": decision.answers,
        "facts": decision.facts,
        "window_digest": evidence.window.window_digest,
        "window_message_count": evidence.window.message_count,
        "relevance_score": entry.envelope.candidate.relevance_score,
        "delivery_outcome": receipt.outcome,
    });
    Some(AnalyticsEventInsert {
        provider: intent_analytics_provider(&evidence.provider_id),
        project_id: project_key.to_owned(),
        session_id: Some(evidence.session_id.as_str().to_owned()),
        timestamp: receipt.delivered_at.0.div_euclid(1_000_000),
        event_kind: "hint_emitted".to_owned(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: Some(decision.selected.as_key().to_owned()),
        hint_id: Some(hex::encode(entry.envelope.envelope_id)),
        outcome: None,
        metadata_json: Some(metadata.to_string()),
    })
}

/// Runs one intent pass. The engine is asked at most once.
#[tracing::instrument(
    name = "hosts.agent.context_scout.intent_pass",
    level = "trace",
    skip_all
)]
pub async fn run_context_scout_intent_pass(
    pass: ContextScoutIntentPassV1<'_>,
) -> ContextScoutIntentPassOutcomeV1 {
    let configuration = pass.pin.configuration();
    let control = configuration.control();
    let Some(settings) = configuration
        .intent_settings()
        .filter(|settings| settings.enabled)
    else {
        return ContextScoutIntentPassOutcomeV1::Disabled;
    };
    match control.state {
        ContextScoutServiceStateV1::Disabled => return ContextScoutIntentPassOutcomeV1::Disabled,
        ContextScoutServiceStateV1::Paused => {
            pass.owner
                .record_intent_outcome(ContextScoutSuppressionV1::Paused)
                .await;
            return ContextScoutIntentPassOutcomeV1::Suppressed {
                reason: ContextScoutSuppressionV1::Paused,
                model_outcome: ContextScoutModelOutcomeV1::NotRequested,
            };
        }
        ContextScoutServiceStateV1::Active => {}
    }
    let Some(code_generation_id) = pass.code_generation_id.clone() else {
        return ContextScoutIntentPassOutcomeV1::CodeGenerationUnavailable;
    };
    let envelope = pass.hook.envelope();
    let Some(provider) = intent_session_provider(envelope.producer) else {
        return ContextScoutIntentPassOutcomeV1::ProviderUnsupported;
    };
    let Ok(provider_id) = ProviderId::new(provider.id()) else {
        return ContextScoutIntentPassOutcomeV1::ProviderUnsupported;
    };
    let Some(intent_lifecycle) = pass.lifecycle.intent_scoped() else {
        return ContextScoutIntentPassOutcomeV1::AddressUnavailable;
    };
    let address = match pass
        .registry
        .bind(pass.hook, pass.pin, intent_lifecycle.clone())
        .await
    {
        ContextScoutAddressBindOutcomeV1::Bound(address)
        | ContextScoutAddressBindOutcomeV1::Existing(address) => address,
        ContextScoutAddressBindOutcomeV1::Conflict
        | ContextScoutAddressBindOutcomeV1::Unavailable => {
            return ContextScoutIntentPassOutcomeV1::AddressUnavailable;
        }
    };
    let Ok(session) = pass
        .owner
        .intent_session_state(
            address,
            pass.observed_at,
            i64::from(settings.min_delivery_gap_secs).saturating_mul(1_000_000),
        )
        .await
    else {
        return ContextScoutIntentPassOutcomeV1::Unavailable;
    };

    let window = match read_boundary_window(&pass, settings, provider.id()).await {
        SessionWindowOutcomeV1::Ready(window) => window,
        SessionWindowOutcomeV1::Empty => return ContextScoutIntentPassOutcomeV1::WindowEmpty,
        SessionWindowOutcomeV1::Unavailable(reason) => {
            tracing::debug!(%reason, "Context Scout intent window unavailable");
            return ContextScoutIntentPassOutcomeV1::WindowUnavailable;
        }
    };
    if matches!(window.freshness, SessionWindowFreshnessV1::Stale { .. }) {
        return ContextScoutIntentPassOutcomeV1::WindowStale;
    }
    if pass.cancellation.is_cancelled() {
        return ContextScoutIntentPassOutcomeV1::Cancelled;
    }

    let engine_identity = pass.engine.as_ref().map(|engine| engine.identity());
    // Facts are pure string inspection over the window, independent of the
    // engine; the engine exchange is the only bounded wait of the pass.
    let facts = window_action_facts(&window);
    let engine_outcome = match ask_engine(&pass, settings, &window).await {
        EngineCallV1::Cancelled => return ContextScoutIntentPassOutcomeV1::Cancelled,
        EngineCallV1::Finished(outcome) => outcome,
    };
    let model_outcome = engine_outcome.model_outcome();
    let answers = engine_outcome.answers();

    let decision = decide_intents(&IntentDecisionInputV1 {
        answers: answers.as_deref(),
        facts: &facts,
        questions: &settings.questions,
        action_fact_score_permille: settings.action_fact_score_permille,
        delivered: &session.delivered_intents,
    });
    let selected = match decision {
        IntentDecisionV1::Candidates(candidates) if !candidates.is_empty() => candidates[0],
        IntentDecisionV1::Candidates(_)
        | IntentDecisionV1::BelowThreshold { .. }
        | IntentDecisionV1::NoSignal => {
            let duplicate = delivered_duplicate(answers.as_deref(), &facts, settings, &session);
            let suppression = duplicate.map(|_| ContextScoutSuppressionV1::Duplicate);
            pass.owner
                .record_intent_outcome(
                    suppression.unwrap_or(ContextScoutSuppressionV1::NoEligibleCandidate),
                )
                .await;
            if let Some(intent) = duplicate {
                record_withheld(&pass, "suppressed_duplicate", intent, &provider_id, None).await;
                return ContextScoutIntentPassOutcomeV1::Suppressed {
                    reason: ContextScoutSuppressionV1::Duplicate,
                    model_outcome,
                };
            }
            return ContextScoutIntentPassOutcomeV1::NoCandidate { model_outcome };
        }
    };

    let delivery_window =
        select_context_scout_delivery_window(&ContextScoutDeliverySelectionInputV1 {
            trigger: ContextScoutTriggerV1::IntentBoundary,
            quiet_mode: false,
            has_recent_delivery: session.delivered_within_gap,
            // A sibling still pending is refused as `PendingSibling`, and spacing
            // after a displayed suggestion is the delivery gap above.
            has_unresolved_interaction: false,
            critical_safety_evidence: false,
            delivered_dedupe_keys: session.delivered_dedupe_keys.clone(),
        });
    let Some(envelope_id) = intent_envelope_id(pass.hook) else {
        return ContextScoutIntentPassOutcomeV1::Unavailable;
    };
    if !matches!(
        delivery_window,
        ContextScoutDeliveryWindowV1::Immediate | ContextScoutDeliveryWindowV1::NextBoundary
    ) {
        // An entry only an idle-window claim could take would also make the
        // session's hook claim ambiguous; spacing withholds it instead.
        pass.owner
            .record_intent_outcome(ContextScoutSuppressionV1::QuietOrUnreceptive)
            .await;
        record_withheld(
            &pass,
            "suppressed_budget",
            selected.intent,
            &provider_id,
            Some(envelope_id),
        )
        .await;
        return ContextScoutIntentPassOutcomeV1::Suppressed {
            reason: ContextScoutSuppressionV1::QuietOrUnreceptive,
            model_outcome,
        };
    }

    let Some(candidate) = intent_candidate(
        &pass,
        settings,
        IntentEvidenceV1 {
            provider_id: provider_id.clone(),
            window: &window,
            code_generation_id,
            engine_identity: engine_identity.as_ref(),
            engine_outcome: &engine_outcome,
            facts: &facts,
            selected,
        },
    ) else {
        return ContextScoutIntentPassOutcomeV1::Unavailable;
    };
    let Some(input_watermark) = digest_bytes(canonical_sha256(&(
        INTENT_WATERMARK_DOMAIN,
        window.window_digest,
        control.configuration_revision,
        selected.intent,
    ))) else {
        return ContextScoutIntentPassOutcomeV1::Unavailable;
    };
    let input = ContextScoutSelectionInputV1 {
        address,
        input_watermark,
        configuration_revision: control.configuration_revision,
        envelope_id,
        now: pass.observed_at,
        delivery_window,
        delivered_dedupe_keys: session.delivered_dedupe_keys,
        candidates: vec![candidate],
    };
    let prepared = pass
        .owner
        .prepare_intent(&input, pass.deadline, pass.cancellation.clone())
        .await
        .map(|outcome| match outcome {
            // The store kept the copy of this suggestion it already claimed
            // or delivered to the session.
            ContextScoutRuntimeOutcomeV1::Enqueued {
                store_outcome: ContextScoutDurableStoreOutcomeV1::Duplicate,
                ..
            } => ContextScoutRuntimeOutcomeV1::Suppressed {
                reason: ContextScoutSuppressionV1::Duplicate,
            },
            outcome => outcome,
        });
    match prepared {
        Ok(ContextScoutRuntimeOutcomeV1::Enqueued {
            entry,
            store_outcome: ContextScoutDurableStoreOutcomeV1::Stored,
        }) => ContextScoutIntentPassOutcomeV1::Enqueued {
            entry,
            lifecycle: Box::new(intent_lifecycle),
        },
        Ok(ContextScoutRuntimeOutcomeV1::Suppressed { reason }) => {
            let withheld = match reason {
                ContextScoutSuppressionV1::Duplicate => Some("suppressed_duplicate"),
                ContextScoutSuppressionV1::PendingSibling => Some("suppressed_budget"),
                _ => None,
            };
            if let Some(event_kind) = withheld {
                record_withheld(
                    &pass,
                    event_kind,
                    selected.intent,
                    &provider_id,
                    Some(envelope_id),
                )
                .await;
            }
            ContextScoutIntentPassOutcomeV1::Suppressed {
                reason,
                model_outcome,
            }
        }
        Ok(
            ContextScoutRuntimeOutcomeV1::Enqueued { .. }
            | ContextScoutRuntimeOutcomeV1::Unavailable,
        )
        | Err(_) => ContextScoutIntentPassOutcomeV1::Unavailable,
    }
}

/// Reads the window, waiting up to the configured freshen budget for the
/// boundary's own turn to land in the transcript store (hosts and the
/// historical ingestor write it; the pass never ingests on its own).
async fn read_boundary_window(
    pass: &ContextScoutIntentPassV1<'_>,
    settings: &ContextScoutIntentSettingsV1,
    provider: &str,
) -> SessionWindowOutcomeV1 {
    let now_micros = u64::try_from(pass.observed_at.0).unwrap_or(0);
    let request = SessionWindowRequestV1 {
        provider,
        session_id: pass.lifecycle.session_id.as_str(),
        limits: SessionWindowLimitsV1 {
            max_user_turns: usize::from(settings.window.max_user_turns),
            max_messages: usize::from(settings.window.max_messages),
            max_bytes: usize::try_from(settings.window.max_bytes).unwrap_or(usize::MAX),
            max_age_micros: Some(u64::from(settings.window.max_age_secs).saturating_mul(1_000_000)),
        },
        now_micros,
    };
    let freshen_until =
        Instant::now() + Duration::from_millis(u64::from(settings.window.freshen_budget_ms));
    let boundary_at = pass.hook.envelope().observed_at.0;
    loop {
        let outcome = read_session_window(pass.sessions, &request).await;
        let caught_up = match &outcome {
            SessionWindowOutcomeV1::Ready(window) => {
                window.newest_timestamp_micros.is_some_and(|newest| {
                    newest.saturating_add(BOUNDARY_TIMESTAMP_SLACK_MICROS) >= boundary_at
                })
            }
            SessionWindowOutcomeV1::Empty => false,
            SessionWindowOutcomeV1::Unavailable(_) => true,
        };
        let next = Instant::now() + FRESHEN_POLL;
        if caught_up
            || next > freshen_until
            || pass.deadline.is_elapsed_at(next)
            || pass.cancellation.is_cancelled()
            || native_runtime::sleep_until(next).await.is_err()
        {
            return outcome;
        }
    }
}

enum EngineCallV1 {
    Finished(EngineOutcomeV1),
    Cancelled,
}

/// What the pass knows about the engine exchange.
enum EngineOutcomeV1 {
    /// No question is configured, so the engine was not asked.
    NotRequested,
    /// The configured engine was refused when the configuration installed.
    Unavailable,
    /// The pass deadline elapsed before the engine answered.
    DeadlineExceeded,
    Asked(DecisionEngineOutcomeV1),
}

impl EngineOutcomeV1 {
    fn model_outcome(&self) -> ContextScoutModelOutcomeV1 {
        match self {
            Self::NotRequested => ContextScoutModelOutcomeV1::NotRequested,
            Self::Unavailable => ContextScoutModelOutcomeV1::Unavailable,
            Self::DeadlineExceeded => ContextScoutModelOutcomeV1::DeadlineExceeded,
            Self::Asked(outcome) => match outcome {
                DecisionEngineOutcomeV1::Answered(_) => ContextScoutModelOutcomeV1::Succeeded,
                DecisionEngineOutcomeV1::Unreachable => ContextScoutModelOutcomeV1::Unavailable,
                DecisionEngineOutcomeV1::TimedOut => ContextScoutModelOutcomeV1::DeadlineExceeded,
                DecisionEngineOutcomeV1::Refused { .. } => ContextScoutModelOutcomeV1::Denied,
                DecisionEngineOutcomeV1::Malformed => ContextScoutModelOutcomeV1::InvalidOutput,
                DecisionEngineOutcomeV1::Backpressured => ContextScoutModelOutcomeV1::Backpressured,
            },
        }
    }

    /// Yes/no probabilities per intent, `None` unless the engine answered.
    /// Keys that name no intent are not answers.
    fn answers(&self) -> Option<Vec<(ContextScoutIntentKindV1, f64)>> {
        let Self::Asked(DecisionEngineOutcomeV1::Answered(answers)) = self else {
            return None;
        };
        Some(
            answers
                .answers
                .iter()
                .filter_map(|(key, answer)| match answer {
                    DecisionAnswerV1::Noul(probability) => {
                        Some((ContextScoutIntentKindV1::from_key(key)?, *probability))
                    }
                    DecisionAnswerV1::Choice { .. } | DecisionAnswerV1::Score(_) => None,
                })
                .collect(),
        )
    }

    fn reported_model(&self) -> Option<&str> {
        match self {
            Self::Asked(DecisionEngineOutcomeV1::Answered(answers)) => answers.model.as_deref(),
            _ => None,
        }
    }
}

/// Asks every configured question in one exchange, bounded by the pass
/// deadline and cancellation. The engine's own timeout bounds the request.
async fn ask_engine(
    pass: &ContextScoutIntentPassV1<'_>,
    settings: &ContextScoutIntentSettingsV1,
    window: &SessionWindowV1,
) -> EngineCallV1 {
    if settings.questions.is_empty() {
        return EngineCallV1::Finished(EngineOutcomeV1::NotRequested);
    }
    let Some(engine) = pass.engine.clone() else {
        return EngineCallV1::Finished(EngineOutcomeV1::Unavailable);
    };
    let request = DecisionRequestV1 {
        state: render_window_state(
            window,
            None,
            usize::try_from(settings.engine.max_state_bytes).unwrap_or(usize::MAX),
        ),
        questions: settings
            .questions
            .iter()
            .map(|question| DecisionQuestionV1 {
                key: question.intent.as_key().to_owned(),
                kind: DecisionQuestionKindV1::Noul,
                instructions: question.instructions.clone(),
            })
            .collect(),
    };
    let asked = native_runtime::spawn_blocking(move || engine.ask_blocking(&request));
    tokio::select! {
        biased;
        () = pass.cancellation.cancelled() => EngineCallV1::Cancelled,
        answered = native_runtime::timeout_at(pass.deadline.instant(), asked) => {
            EngineCallV1::Finished(match answered {
                Ok(Ok(outcome)) => EngineOutcomeV1::Asked(outcome),
                // A lost blocking worker reads as an unreachable engine.
                Ok(Err(_)) => EngineOutcomeV1::Asked(DecisionEngineOutcomeV1::Unreachable),
                Err(_) => EngineOutcomeV1::DeadlineExceeded,
            })
        }
    }
}

/// Action facts of the boundary's turn: the newest user message and every
/// tool call recorded after it. Earlier turns were judged by earlier passes.
fn window_action_facts(window: &SessionWindowV1) -> Vec<ContextScoutIntentKindV1> {
    let turn_start = window
        .messages
        .iter()
        .rposition(|message| message.role == SessionWindowRoleV1::User);
    let mut facts = Vec::new();
    let mut push = |found: Vec<ContextScoutIntentKindV1>| {
        for intent in found {
            if !facts.contains(&intent) {
                facts.push(intent);
            }
        }
    };
    let turn = match turn_start {
        Some(index) => {
            let prompt = window.messages[index].text.as_str();
            if !prompt.is_empty() {
                push(action_facts(&ActionFactInputV1 {
                    prompt: Some(prompt),
                    ..ActionFactInputV1::default()
                }));
            }
            &window.messages[index + 1..]
        }
        None => window.messages.as_slice(),
    };
    for message in turn {
        for call in &message.tool_calls {
            push(tool_call_facts(
                &call.name,
                call.argument_excerpt.as_deref(),
            ));
        }
    }
    facts
}

fn tool_call_facts(
    tool_name: &str,
    argument_excerpt: Option<&str>,
) -> Vec<ContextScoutIntentKindV1> {
    // An excerpt that is still whole JSON names its arguments; a truncated
    // one leaves only the tool's shape.
    let arguments = argument_excerpt
        .and_then(|excerpt| serde_json::from_str::<serde_json::Value>(excerpt).ok())
        .filter(serde_json::Value::is_object);
    let text_argument = |keys: &[&str]| {
        arguments.as_ref().and_then(|arguments| {
            keys.iter()
                .find_map(|key| arguments.get(*key).and_then(serde_json::Value::as_str))
                .map(str::to_owned)
        })
    };
    let command = text_argument(&["command", "cmd"]);
    let file_path = text_argument(&["file_path", "path", "filePath", "target_file"]);
    let subagent_type = text_argument(&["subagent_type"]);
    action_facts(&ActionFactInputV1 {
        tool_name: Some(tool_name),
        command: command.as_deref(),
        file_path: file_path.as_deref(),
        subagent_type: subagent_type.as_deref(),
        ..ActionFactInputV1::default()
    })
}

/// The intent an already-delivered suggestion withheld: the decision with
/// the session's deliveries ignored would have named it first.
fn delivered_duplicate(
    answers: Option<&[(ContextScoutIntentKindV1, f64)]>,
    facts: &[ContextScoutIntentKindV1],
    settings: &ContextScoutIntentSettingsV1,
    session: &ContextScoutIntentSessionStateV1,
) -> Option<ContextScoutIntentKindV1> {
    if session.delivered_intents.is_empty() {
        return None;
    }
    match decide_intents(&IntentDecisionInputV1 {
        answers,
        facts,
        questions: &settings.questions,
        action_fact_score_permille: settings.action_fact_score_permille,
        delivered: &BTreeSet::new(),
    }) {
        IntentDecisionV1::Candidates(candidates) => candidates
            .first()
            .map(|candidate| candidate.intent)
            .filter(|intent| session.delivered_intents.contains(intent)),
        IntentDecisionV1::BelowThreshold { .. } | IntentDecisionV1::NoSignal => None,
    }
}

struct IntentEvidenceV1<'a> {
    provider_id: ProviderId,
    window: &'a SessionWindowV1,
    code_generation_id: CodeGenerationId,
    engine_identity: Option<&'a super::decision_engine::DecisionEngineIdentityV1>,
    engine_outcome: &'a EngineOutcomeV1,
    facts: &'a [ContextScoutIntentKindV1],
    selected: IntentCandidateV1,
}

/// Builds the one session-evidence candidate of the pass.
fn intent_candidate(
    pass: &ContextScoutIntentPassV1<'_>,
    settings: &ContextScoutIntentSettingsV1,
    evidence: IntentEvidenceV1<'_>,
) -> Option<ContextScoutCandidateV1> {
    let IntentEvidenceV1 {
        provider_id,
        window,
        code_generation_id,
        engine_identity,
        engine_outcome,
        facts,
        selected,
    } = evidence;
    let model_outcome = engine_outcome.model_outcome();
    let answers = if model_outcome == ContextScoutModelOutcomeV1::Succeeded {
        let answered = engine_outcome.answers().unwrap_or_default();
        settings
            .questions
            .iter()
            .filter_map(|question| {
                let probability = answered
                    .iter()
                    .find(|(intent, _)| *intent == question.intent)
                    .map(|(_, probability)| *probability)
                    .filter(|probability| probability.is_finite())?;
                Some(ContextScoutIntentAnswerV1 {
                    intent: question.intent,
                    probability_permille: probability_permille(probability),
                    threshold_permille: question.threshold_permille,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let engine = match engine_identity {
        Some(identity) => Some(ReceiptEngineIdentityV1 {
            base_url_digest: ManifestDigest::from_sha256_bytes(&identity.base_url_digest).ok()?,
            model: engine_outcome
                .reported_model()
                .filter(|model| receipt_model_is_valid(model))
                .map(str::to_owned)
                .or_else(|| identity.model.clone()),
        }),
        None => None,
    };
    let policy = PolicyDecisionRef::new(
        INTENT_POLICY_ID,
        pass.context.grant().revision,
        pass.context.grant().digest.clone(),
        ComponentVersion::new(INTENT_POLICY_VERSION).ok()?,
    )
    .ok()?;
    let authority = AuthorityReceipt::from_context(pass.context, policy, pass.observed_at).ok()?;
    let redaction = ContextScoutRedactionReceiptV1::MetadataOnly {
        disclosure: authority.disclosure,
    };
    let session_evidence = ContextScoutSessionEvidenceV1::claim(ContextScoutSessionClaimInputV1 {
        authorized_scope: pass.context.scope().clone(),
        provider_id,
        session_id: pass.lifecycle.session_id.clone(),
        protected_session_id: pass.hook.envelope().protected_session_id,
        window: ContextScoutSessionWindowV1 {
            first_store_id: window.first_store_id,
            last_store_id: window.last_store_id,
            message_count: u16::try_from(window.messages.len()).ok()?,
            window_digest: ManifestDigest::from_sha256_bytes(&window.window_digest).ok()?,
            newest_timestamp: window.newest_timestamp_micros,
        },
        prompt_excerpt_digest: None,
        code_generation_id: Some(code_generation_id),
        authority,
        redaction,
        availability: ContextScoutEvidenceAvailabilityV1::Complete,
        decision: ContextScoutIntentReceiptV1 {
            engine,
            outcome: model_outcome,
            answers,
            facts: facts.to_vec(),
            selected: selected.intent,
            source: selected.source,
        },
        claimed_at: pass.observed_at,
    })
    .ok()?;
    let dedupe_key = digest_bytes(canonical_sha256(&(
        INTENT_CANDIDATE_DEDUPE_DOMAIN,
        selected.intent,
        pass.hook.envelope().protected_session_id,
    )))?;
    Some(ContextScoutCandidateV1 {
        dedupe_key,
        category: ContextScoutCategoryV1::Retrieval,
        relevance_score: selected.relevance,
        suggestion_text: suggestion_text(selected.intent),
        evidence: session_evidence.into(),
        expires_at: UtcMicros(
            pass.observed_at
                .0
                .saturating_add(i64::from(settings.ttl_secs).saturating_mul(1_000_000)),
        ),
    })
}

/// Envelope identity of the pass's suggestion. Derived from the boundary so a
/// stop boundary's intent suggestion never shares the feedback suggestion's
/// envelope identity.
fn intent_envelope_id(hook: &AdmittedContextScoutHookV1) -> Option<[u8; 16]> {
    let envelope = hook.envelope();
    let digest = digest_bytes(canonical_sha256(&(
        INTENT_ENVELOPE_DOMAIN,
        envelope.project_id,
        envelope.event_id,
    )))?;
    let mut envelope_id = [0; 16];
    envelope_id.copy_from_slice(&digest[..16]);
    (envelope_id != [0; 16]).then_some(envelope_id)
}

/// Records one suggestion the pass decided and withheld, with the same
/// identity a delivery would carry.
async fn record_withheld(
    pass: &ContextScoutIntentPassV1<'_>,
    event_kind: &str,
    intent: ContextScoutIntentKindV1,
    provider_id: &ProviderId,
    envelope_id: Option<[u8; 16]>,
) {
    let Some(analytics) = pass.analytics.as_ref() else {
        return;
    };
    let Some(envelope_id) = envelope_id.or_else(|| intent_envelope_id(pass.hook)) else {
        return;
    };
    let event = AnalyticsEventInsert {
        provider: intent_analytics_provider(provider_id),
        project_id: analytics.project_key.to_owned(),
        session_id: Some(pass.lifecycle.session_id.as_str().to_owned()),
        timestamp: pass.observed_at.0.div_euclid(1_000_000),
        event_kind: event_kind.to_owned(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: Some(intent.as_key().to_owned()),
        hint_id: Some(hex::encode(envelope_id)),
        outcome: None,
        metadata_json: Some(
            serde_json::json!({
                "source": "context_scout_intent_pass",
                "route": "decision_engine",
            })
            .to_string(),
        ),
    };
    if let Err(error) = analytics.analytics.append_analytics_events(&[event]).await {
        tracing::debug!(%error, event_kind, "Context Scout intent analytics not recorded");
    }
}

fn receipt_model_is_valid(model: &str) -> bool {
    !model.trim().is_empty()
        && model.len() <= MAX_DECISION_ENGINE_MODEL_BYTES
        && !model.chars().any(char::is_control)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn probability_permille(probability: f64) -> u16 {
    (probability.clamp(0.0, 1.0) * f64::from(INTENT_MAX_PERMILLE)).round() as u16
}

fn digest_bytes(
    digest: Result<ManifestDigest, tracedecay_domain::DomainError>,
) -> Option<[u8; 32]> {
    let digest = digest.ok()?;
    let mut bytes = [0; 32];
    hex::decode_to_slice(digest.hex_suffix()?, &mut bytes).ok()?;
    Some(bytes)
}
