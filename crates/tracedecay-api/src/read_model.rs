//! Typed presentation contract shared by the PR14 V2 read-model routes.
//!
//! This module is the **generation source** for the dashboard frontend's
//! `contracts/` wire boundary (docs/plans/tracedecay-v2/11-dashboard-frontend.md
//! §"Typed presentation contracts"). Every V2 read-model response is a
//! [`DashboardEnvelopeV1<T>`] carrying the normative envelope shape: schema
//! revision, exact scope, entity/graph version, valid and observation time,
//! source watermark, authorization, coverage, freshness, domain state (the
//! closed [`DashboardDomainStateV1`] union), legal action references, and the
//! typed payload.
//!
//! Truthfulness invariants from the plan are encoded structurally, not by
//! convention:
//! - Unknown denominators never render as complete: [`DashboardCoverageV1`]'s
//!   only "complete" constructor requires a known denominator, and
//!   [`DashboardCoverageCompletenessV1::Complete`] is unreachable without it.
//! - Absent sources are typed absent/unsupported: [`DashboardDomainStateV1`]
//!   carries an explicit [`DashboardDomainStateV1::Unsupported`] variant for a
//!   read model whose live producer is not yet wired server-side (plan §"Known
//!   backend gaps"), so a missing source never collapses into `ready` or a
//!   default `complete_zero_findings`.
//! - `complete_zero_findings` is only legal with genuinely complete coverage;
//!   see [`DashboardEnvelopeV1::complete_zero_findings`], which requires a
//!   [`DashboardCoverageV1`] built from the complete constructor.
//!
//! Every enum is `#[serde(rename_all = "snake_case")]` and closed; new variants
//! are added through a future versioned type rather than by widening an existing
//! variant, so the frontend's exhaustive `never`-checked switches stay honest.
//!
//! The executable resolves the exact [`DashboardScopeV1`] from its own live
//! composition state; this crate never reads scope from a path or a store.

use schemars::JsonSchema;
use serde::Serialize;

pub mod multi_root;

/// Schema revision of the envelope contract. The frontend refuses to decode a
/// higher revision it was not generated against and renders `unsupported_schema`.
pub const DASHBOARD_SCHEMA_REVISION_V1: u32 = 1;

/// The normative dashboard domain-state union.
///
/// The first sixteen variants are the plan's exact `DashboardDomainState`
/// discriminated union. [`Self::Unsupported`] is the PR14 backend-gap binding
/// state: the read model's HTTP surface exists, but its live producer/source is
/// not yet wired server-side. It is never healthy or empty — the frontend
/// renders a distinct "not yet available" state — and it is deliberately
/// separate from [`Self::UnsupportedSchema`] (an undecodable schema/variant).
// The full sixteen-state union plus `Unsupported` is normative contract (the
// generation source for the frontend's exhaustive switches). Most variants are
// not yet emitted by a server-side read but must exist in the generated union.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DashboardDomainStateV1 {
    Loading,
    CompleteZeroFindings,
    Ready,
    Partial,
    Stale,
    Locked,
    Denied,
    Unauthorized,
    Redacted,
    Conflicting,
    Offline,
    Unknown,
    Cancelled,
    TimedOut,
    Error,
    UnsupportedSchema,
    /// The read model exists but its live producer/source is not yet wired
    /// server-side (plan §"Known backend gaps"). Distinct from every "healthy"
    /// or "empty" state.
    Unsupported,
}

/// Exact scope the envelope was resolved for. A deep link/query never falls
/// back to a title, path, or latest version to recover scope.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DashboardScopeV1 {
    /// Registered project id, when the store is profile-backed.
    pub project_id: Option<String>,
    /// Resolved storage mode label (`project_local` / `profile_sharded`).
    pub storage_mode: String,
    /// Resolved active project store root (display path).
    pub store_root: String,
}

/// Entity and graph version identities pinned by the envelope. Both are
/// optional: a read model with no versioned graph state leaves them absent
/// rather than inventing `0`/`latest`.
#[derive(Clone, Debug, Default, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DashboardVersionV1 {
    pub entity_version: Option<String>,
    pub graph_version: Option<String>,
}

/// Valid time and observation time, kept separate. `observation_time` is when
/// the daemon observed the state; `valid_time` is when the state was true in the
/// modelled domain (absent when a read model has no distinct valid time).
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DashboardTimeV1 {
    /// Domain valid time in microseconds since the Unix epoch, when distinct.
    pub valid_time_micros: Option<i64>,
    /// Observation time in microseconds since the Unix epoch. Always present.
    pub observation_time_micros: i64,
}

impl DashboardTimeV1 {
    /// Observation-only timing stamped at the current wall clock.
    #[must_use]
    pub fn observed_now() -> Self {
        Self {
            valid_time_micros: None,
            observation_time_micros: now_micros(),
        }
    }
}

/// Opaque monotone source watermark. The frontend compares watermarks for
/// staleness but never parses their internal structure.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DashboardWatermarkV1 {
    /// Which source the watermark belongs to.
    pub source: String,
    /// Opaque monotone token.
    pub watermark: String,
}

/// Authorization outcome for the read. On the loopback single-user dashboard a
/// legal local read is [`Self::Authorized`]; the other variants are retained so
/// the contract can express `unauthorized` (identity absent/expired), `denied`
/// (known identity lacks permission), and `redacted` reads without a schema
/// change.
// The full authorization vocabulary is normative contract; only `Authorized` is
// constructed by the local loopback dashboard today.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum DashboardAuthorizationV1 {
    Authorized,
    Unauthorized,
    Denied,
    Redacted,
}

/// Coverage completeness axis. `Unsupported` distinguishes "the source that
/// would establish coverage is not wired" from `Unknown` ("coverage could not
/// be determined").
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DashboardCoverageCompletenessV1 {
    Complete,
    Partial,
    Unknown,
    Unsupported,
}

/// Coverage statement. Counts are optional; an unknown denominator is `None`,
/// never a fabricated `0`/`100%`. The completeness axis is authoritative — the
/// frontend never derives `complete` from a `matched == eligible` coincidence.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DashboardCoverageV1 {
    pub completeness: DashboardCoverageCompletenessV1,
    pub eligible: Option<u64>,
    pub examined: Option<u64>,
    pub matched: Option<u64>,
    pub excluded: Option<u64>,
    pub omitted: Option<u64>,
    pub unknown: Option<u64>,
    /// Denominator for a percentage. `None` means the denominator is unknown, so
    /// the frontend must not render a meter or a percentage.
    pub denominator: Option<u64>,
    pub unit: Option<String>,
    pub omission_reasons: Vec<String>,
}

impl DashboardCoverageV1 {
    /// Coverage whose completeness could not be determined. No denominator, so
    /// no percentage/meter can render.
    #[must_use]
    pub fn unknown() -> Self {
        Self::bare(DashboardCoverageCompletenessV1::Unknown)
    }

    /// Coverage over a source that is not wired server-side.
    #[must_use]
    pub fn unsupported() -> Self {
        Self::bare(DashboardCoverageCompletenessV1::Unsupported)
    }

    /// Complete coverage over a **known** denominator of `eligible` units, all
    /// of which were examined. This is the only constructor that can produce
    /// [`DashboardCoverageCompletenessV1::Complete`], so a complete claim always
    /// carries a real denominator.
    #[must_use]
    pub fn complete(eligible: u64, unit: impl Into<String>) -> Self {
        Self {
            completeness: DashboardCoverageCompletenessV1::Complete,
            eligible: Some(eligible),
            examined: Some(eligible),
            matched: Some(eligible),
            excluded: Some(0),
            omitted: Some(0),
            unknown: Some(0),
            denominator: Some(eligible),
            unit: Some(unit.into()),
            omission_reasons: Vec::new(),
        }
    }

    /// Partial coverage: `examined` of a known `eligible` denominator, with the
    /// remainder omitted for the stated reasons.
    #[must_use]
    pub fn partial(
        eligible: u64,
        examined: u64,
        unit: impl Into<String>,
        omission_reasons: Vec<String>,
    ) -> Self {
        Self {
            completeness: DashboardCoverageCompletenessV1::Partial,
            eligible: Some(eligible),
            examined: Some(examined),
            matched: None,
            excluded: None,
            omitted: Some(eligible.saturating_sub(examined)),
            unknown: None,
            denominator: Some(eligible),
            unit: Some(unit.into()),
            omission_reasons,
        }
    }

    fn bare(completeness: DashboardCoverageCompletenessV1) -> Self {
        Self {
            completeness,
            eligible: None,
            examined: None,
            matched: None,
            excluded: None,
            omitted: None,
            unknown: None,
            denominator: None,
            unit: None,
            omission_reasons: Vec::new(),
        }
    }

    /// True only for genuinely complete coverage over a known denominator.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.completeness, DashboardCoverageCompletenessV1::Complete)
            && self.denominator.is_some()
    }
}

/// Freshness of the observed state relative to its live source watermark.
/// `Absent` (no source produced anything) and `Unsupported` (no source wired)
/// are distinct from `Stale` (behind the watermark) and `Unknown`.
// `Stale`/`Absent` are normative freshness states not yet emitted by the current
// read sources.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DashboardFreshnessStateV1 {
    Fresh,
    Stale,
    Unknown,
    Absent,
    Unsupported,
}

/// Freshness statement plus the optional observation stamp/watermark it was
/// judged against.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DashboardFreshnessV1 {
    pub state: DashboardFreshnessStateV1,
    pub observed_at_micros: Option<i64>,
    pub watermark: Option<String>,
}

impl DashboardFreshnessV1 {
    #[must_use]
    pub fn fresh_now() -> Self {
        Self {
            state: DashboardFreshnessStateV1::Fresh,
            observed_at_micros: Some(now_micros()),
            watermark: None,
        }
    }

    /// Behind the live source watermark, stamped at the current observation.
    #[must_use]
    pub fn stale_now() -> Self {
        Self {
            state: DashboardFreshnessStateV1::Stale,
            observed_at_micros: Some(now_micros()),
            watermark: None,
        }
    }

    #[must_use]
    pub fn unknown() -> Self {
        Self {
            state: DashboardFreshnessStateV1::Unknown,
            observed_at_micros: None,
            watermark: None,
        }
    }

    #[must_use]
    pub fn unsupported() -> Self {
        Self {
            state: DashboardFreshnessStateV1::Unsupported,
            observed_at_micros: None,
            watermark: None,
        }
    }
}

/// The legal-action reference kinds a read model may attach. This mirrors the
/// plan's action vocabulary reduced to the read surface: the dashboard only
/// renders these references and submits them through the owning application
/// operation; it never constructs an effect inline.
// The full action vocabulary is part of the normative contract (the generation
// source for the frontend). Variants beyond `Refresh` are not yet constructed
// server-side but must exist in the generated union.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DashboardLegalActionKindV1 {
    Inspect,
    ExpandEvidence,
    Refresh,
    RequestDryRun,
    RequestApply,
    RequestCancel,
}

/// A reference to one owner-supplied legal action. `operation` names the owning
/// application operation; the dashboard never embeds argv, a path, or an inline
/// effect.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DashboardLegalActionRefV1 {
    pub kind: DashboardLegalActionKindV1,
    pub operation: String,
}

impl DashboardLegalActionRefV1 {
    #[must_use]
    pub fn new(kind: DashboardLegalActionKindV1, operation: impl Into<String>) -> Self {
        Self {
            kind,
            operation: operation.into(),
        }
    }
}

/// The normative read-model envelope. Every V2 read-model route returns exactly
/// this shape; only `payload` varies by route.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DashboardEnvelopeV1<T> {
    pub schema_revision: u32,
    pub scope: DashboardScopeV1,
    pub version: DashboardVersionV1,
    pub time: DashboardTimeV1,
    pub source_watermark: Option<DashboardWatermarkV1>,
    pub authorization: DashboardAuthorizationV1,
    pub coverage: DashboardCoverageV1,
    pub freshness: DashboardFreshnessV1,
    pub domain_state: DashboardDomainStateV1,
    pub legal_actions: Vec<DashboardLegalActionRefV1>,
    pub payload: T,
}

impl<T> DashboardEnvelopeV1<T> {
    /// Base constructor: a locally authorized read with observation-only timing
    /// and no versioned graph identity. Callers refine coverage, freshness,
    /// watermark, legal actions, and version through the builder setters.
    #[must_use]
    pub fn new(
        scope: DashboardScopeV1,
        domain_state: DashboardDomainStateV1,
        coverage: DashboardCoverageV1,
        freshness: DashboardFreshnessV1,
        payload: T,
    ) -> Self {
        Self {
            schema_revision: DASHBOARD_SCHEMA_REVISION_V1,
            scope,
            version: DashboardVersionV1::default(),
            time: DashboardTimeV1::observed_now(),
            source_watermark: None,
            authorization: DashboardAuthorizationV1::Authorized,
            coverage,
            freshness,
            domain_state,
            legal_actions: Vec::new(),
            payload,
        }
    }

    /// A `ready` envelope over complete coverage and fresh state.
    #[must_use]
    pub fn ready(scope: DashboardScopeV1, coverage: DashboardCoverageV1, payload: T) -> Self {
        Self::new(
            scope,
            DashboardDomainStateV1::Ready,
            coverage,
            DashboardFreshnessV1::fresh_now(),
            payload,
        )
    }

    /// An `unsupported` envelope for a read model whose live source is not yet
    /// wired server-side. Coverage and freshness are typed unsupported so no
    /// consumer can read a healthy/empty result out of the absence.
    #[must_use]
    pub fn unsupported(scope: DashboardScopeV1, payload: T) -> Self {
        Self::new(
            scope,
            DashboardDomainStateV1::Unsupported,
            DashboardCoverageV1::unsupported(),
            DashboardFreshnessV1::unsupported(),
            payload,
        )
    }

    /// A mounted read model whose owning source is temporarily unavailable.
    /// This is distinct from `unsupported`: the capability exists, but no
    /// value or denominator may be claimed for this observation.
    #[must_use]
    pub fn unavailable(scope: DashboardScopeV1, payload: T, reason: impl Into<String>) -> Self {
        let mut coverage = DashboardCoverageV1::unknown();
        coverage.omission_reasons.push(reason.into());
        Self::new(
            scope,
            DashboardDomainStateV1::Unknown,
            coverage,
            DashboardFreshnessV1::unknown(),
            payload,
        )
    }

    /// A mounted read whose source failed. The reason is carried as coverage
    /// evidence and coverage/freshness remain unknown; callers must not replace
    /// the unavailable payload with an empty success.
    #[must_use]
    pub fn error(scope: DashboardScopeV1, payload: T, reason: impl Into<String>) -> Self {
        let mut coverage = DashboardCoverageV1::unknown();
        coverage.omission_reasons.push(reason.into());
        Self::new(
            scope,
            DashboardDomainStateV1::Error,
            coverage,
            DashboardFreshnessV1::unknown(),
            payload,
        )
    }

    /// A successful observation that is behind its source watermark.
    #[must_use]
    pub fn stale(scope: DashboardScopeV1, coverage: DashboardCoverageV1, payload: T) -> Self {
        Self::new(
            scope,
            DashboardDomainStateV1::Stale,
            coverage,
            DashboardFreshnessV1::stale_now(),
            payload,
        )
    }

    /// A mounted read that cannot proceed while its canonical authority is
    /// locked. The reason remains coverage evidence; no payload is fabricated.
    #[must_use]
    pub fn locked(scope: DashboardScopeV1, payload: T, reason: impl Into<String>) -> Self {
        let mut coverage = DashboardCoverageV1::unknown();
        coverage.omission_reasons.push(reason.into());
        Self::new(
            scope,
            DashboardDomainStateV1::Locked,
            coverage,
            DashboardFreshnessV1::unknown(),
            payload,
        )
    }

    /// A read whose canonical authority admitted the caller but redacted the
    /// requested content. Redaction is both a domain and authorization state.
    #[must_use]
    pub fn redacted(scope: DashboardScopeV1, payload: T, reason: impl Into<String>) -> Self {
        let mut coverage = DashboardCoverageV1::unknown();
        coverage.omission_reasons.push(reason.into());
        let mut envelope = Self::new(
            scope,
            DashboardDomainStateV1::Redacted,
            coverage,
            DashboardFreshnessV1::unknown(),
            payload,
        );
        envelope.authorization = DashboardAuthorizationV1::Redacted;
        envelope
    }

    /// A partial observation with a known eligible population.
    #[must_use]
    pub fn partial(
        scope: DashboardScopeV1,
        eligible: u64,
        examined: u64,
        unit: impl Into<String>,
        omission_reasons: Vec<String>,
        payload: T,
    ) -> Self {
        Self::new(
            scope,
            DashboardDomainStateV1::Partial,
            DashboardCoverageV1::partial(eligible, examined, unit, omission_reasons),
            DashboardFreshnessV1::unknown(),
            payload,
        )
    }

    /// A known caller without permission. Payload types must use a safe empty
    /// or redacted representation; the envelope never fabricates coverage.
    #[must_use]
    pub fn denied(scope: DashboardScopeV1, payload: T) -> Self {
        let mut envelope = Self::new(
            scope,
            DashboardDomainStateV1::Denied,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            payload,
        );
        envelope.authorization = DashboardAuthorizationV1::Denied;
        envelope
    }

    /// A caller for whom no valid identity was admitted.
    #[must_use]
    pub fn unauthorized(scope: DashboardScopeV1, payload: T) -> Self {
        let mut envelope = Self::new(
            scope,
            DashboardDomainStateV1::Unauthorized,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            payload,
        );
        envelope.authorization = DashboardAuthorizationV1::Unauthorized;
        envelope
    }

    /// A `complete_zero_findings` envelope. Only constructible from complete
    /// coverage — the plan's rule that the empty result is legal only under
    /// genuinely complete coverage is enforced here: a non-complete coverage
    /// argument downgrades the state to `partial` rather than lying.
    #[must_use]
    pub fn complete_zero_findings(
        scope: DashboardScopeV1,
        coverage: DashboardCoverageV1,
        payload: T,
    ) -> Self {
        let (state, freshness) = if coverage.is_complete() {
            (
                DashboardDomainStateV1::CompleteZeroFindings,
                DashboardFreshnessV1::fresh_now(),
            )
        } else {
            (
                DashboardDomainStateV1::Partial,
                DashboardFreshnessV1::unknown(),
            )
        };
        Self::new(scope, state, coverage, freshness, payload)
    }

    #[must_use]
    pub fn with_version(mut self, version: DashboardVersionV1) -> Self {
        self.version = version;
        self
    }

    #[must_use]
    pub fn with_source_watermark(mut self, watermark: DashboardWatermarkV1) -> Self {
        self.source_watermark = Some(watermark);
        self
    }

    #[must_use]
    pub fn with_legal_actions(mut self, actions: Vec<DashboardLegalActionRefV1>) -> Self {
        self.legal_actions = actions;
        self
    }

    #[must_use]
    pub fn map_payload<U>(self, map: impl FnOnce(T) -> U) -> DashboardEnvelopeV1<U> {
        DashboardEnvelopeV1 {
            schema_revision: self.schema_revision,
            scope: self.scope,
            version: self.version,
            time: self.time,
            source_watermark: self.source_watermark,
            authorization: self.authorization,
            coverage: self.coverage,
            freshness: self.freshness,
            domain_state: self.domain_state,
            legal_actions: self.legal_actions,
            payload: map(self.payload),
        }
    }

    #[must_use]
    pub fn with_valid_time(mut self, valid_time_micros: i64) -> Self {
        self.time.valid_time_micros = Some(valid_time_micros);
        self
    }
}

/// Current wall-clock time in microseconds since the Unix epoch.
///
/// Dashboard read models carry raw `i64` micros, so this unwraps the canonical
/// [`tracedecay_application::now_micros`] rather than restating its saturating
/// clamp.
#[must_use]
pub fn now_micros() -> i64 {
    tracedecay_application::now_micros().0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_revision_is_stamped() {
        let envelope = DashboardEnvelopeV1::unsupported(scope(), 7_u32);
        assert_eq!(envelope.schema_revision, DASHBOARD_SCHEMA_REVISION_V1);
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        assert_eq!(
            envelope.coverage.completeness,
            DashboardCoverageCompletenessV1::Unsupported
        );
        assert_eq!(
            envelope.freshness.state,
            DashboardFreshnessStateV1::Unsupported
        );
    }

    #[test]
    fn unavailable_partial_and_denied_never_claim_complete_coverage() {
        let unavailable =
            DashboardEnvelopeV1::unavailable(scope(), (), "source_temporarily_unavailable");
        assert_eq!(unavailable.domain_state, DashboardDomainStateV1::Unknown);
        assert!(!unavailable.coverage.is_complete());
        assert_eq!(
            unavailable.coverage.omission_reasons,
            ["source_temporarily_unavailable"]
        );

        let partial = DashboardEnvelopeV1::partial(
            scope(),
            10,
            4,
            "rows",
            vec!["source_timeout".to_owned()],
            (),
        );
        assert_eq!(partial.domain_state, DashboardDomainStateV1::Partial);
        assert_eq!(partial.coverage.denominator, Some(10));
        assert!(!partial.coverage.is_complete());

        let denied = DashboardEnvelopeV1::denied(scope(), ());
        assert_eq!(denied.domain_state, DashboardDomainStateV1::Denied);
        assert_eq!(denied.authorization, DashboardAuthorizationV1::Denied);
        assert!(!denied.coverage.is_complete());
    }

    #[test]
    fn unavailable_error_keeps_the_reason_and_carries_no_fabricated_payload() {
        let envelope =
            DashboardEnvelopeV1::<Option<u64>>::error(scope(), None, "graph_projection_failed");

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Error);
        assert_eq!(
            envelope.coverage.omission_reasons,
            ["graph_projection_failed"]
        );
        assert_eq!(envelope.freshness.state, DashboardFreshnessStateV1::Unknown);
        assert_eq!(envelope.payload, None);
    }

    #[test]
    fn stale_read_is_never_reported_as_ready_or_fresh() {
        let envelope = DashboardEnvelopeV1::stale(
            scope(),
            DashboardCoverageV1::complete(3, "records"),
            Some(7_u64),
        );

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Stale);
        assert_eq!(envelope.freshness.state, DashboardFreshnessStateV1::Stale);
        assert_eq!(envelope.payload, Some(7));
    }

    #[test]
    fn locked_and_redacted_reads_preserve_their_exact_state() {
        let locked =
            DashboardEnvelopeV1::<Option<u64>>::locked(scope(), None, "temporal_store_locked");
        assert_eq!(locked.domain_state, DashboardDomainStateV1::Locked);
        assert_eq!(locked.coverage.omission_reasons, ["temporal_store_locked"]);
        assert_eq!(locked.authorization, DashboardAuthorizationV1::Authorized);

        let redacted =
            DashboardEnvelopeV1::<Option<u64>>::redacted(scope(), None, "content_redacted");
        assert_eq!(redacted.domain_state, DashboardDomainStateV1::Redacted);
        assert_eq!(redacted.authorization, DashboardAuthorizationV1::Redacted);
        assert_eq!(redacted.coverage.omission_reasons, ["content_redacted"]);
    }

    #[test]
    fn unauthorized_read_is_distinct_from_a_known_denial() {
        let envelope = DashboardEnvelopeV1::<Option<u64>>::unauthorized(scope(), None);

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unauthorized);
        assert_eq!(
            envelope.authorization,
            DashboardAuthorizationV1::Unauthorized
        );
        assert!(!envelope.coverage.is_complete());
        assert_eq!(envelope.payload, None);
    }

    #[test]
    fn complete_coverage_requires_known_denominator() {
        let complete = DashboardCoverageV1::complete(4, "stores");
        assert!(complete.is_complete());
        assert_eq!(complete.denominator, Some(4));

        // Neither unknown nor unsupported coverage can ever be "complete".
        assert!(!DashboardCoverageV1::unknown().is_complete());
        assert!(!DashboardCoverageV1::unsupported().is_complete());
    }

    #[test]
    fn complete_zero_findings_downgrades_without_complete_coverage() {
        let honest = DashboardEnvelopeV1::complete_zero_findings(
            scope(),
            DashboardCoverageV1::unknown(),
            Vec::<u8>::new(),
        );
        assert_eq!(honest.domain_state, DashboardDomainStateV1::Partial);

        let genuine = DashboardEnvelopeV1::complete_zero_findings(
            scope(),
            DashboardCoverageV1::complete(0, "findings"),
            Vec::<u8>::new(),
        );
        assert_eq!(
            genuine.domain_state,
            DashboardDomainStateV1::CompleteZeroFindings
        );
    }

    #[test]
    fn domain_state_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&DashboardDomainStateV1::CompleteZeroFindings).unwrap(),
            "\"complete_zero_findings\""
        );
        assert_eq!(
            serde_json::to_string(&DashboardDomainStateV1::UnsupportedSchema).unwrap(),
            "\"unsupported_schema\""
        );
        assert_eq!(
            serde_json::to_string(&DashboardDomainStateV1::Unsupported).unwrap(),
            "\"unsupported\""
        );
    }

    #[test]
    fn envelope_serializes_full_contract_surface() {
        let envelope = DashboardEnvelopeV1::ready(
            scope(),
            DashboardCoverageV1::complete(1, "stores"),
            json_payload(),
        )
        .with_source_watermark(DashboardWatermarkV1 {
            source: "graph".into(),
            watermark: "wm-1".into(),
        })
        .with_legal_actions(vec![DashboardLegalActionRefV1::new(
            DashboardLegalActionKindV1::Refresh,
            "use-case.dashboard.refresh",
        )]);
        let value = serde_json::to_value(&envelope).unwrap();
        for key in [
            "schema_revision",
            "scope",
            "version",
            "time",
            "source_watermark",
            "authorization",
            "coverage",
            "freshness",
            "domain_state",
            "legal_actions",
            "payload",
        ] {
            assert!(value.get(key).is_some(), "envelope missing `{key}`");
        }
        assert_eq!(value["authorization"]["outcome"], "authorized");
    }

    fn scope() -> DashboardScopeV1 {
        DashboardScopeV1 {
            project_id: Some("proj".into()),
            storage_mode: "profile_sharded".into(),
            store_root: "/store".into(),
        }
    }

    fn json_payload() -> serde_json::Value {
        serde_json::json!({ "ok": true })
    }
}
