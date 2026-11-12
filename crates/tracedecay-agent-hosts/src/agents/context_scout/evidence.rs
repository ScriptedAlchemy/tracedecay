#[cfg(test)]
use tracedecay_contracts::context_scout::ContextScoutEvidenceSourceKindV1;
use tracedecay_contracts::context_scout::{
    ContextScoutCandidateEvidenceV1, ContextScoutEvidenceAvailabilityV1,
    ContextScoutEvidenceEnvelopeV1, ContextScoutEvidenceSourceReceiptV1, ContextScoutIntentKindV1,
    ContextScoutIntentReceiptV1, ContextScoutRedactionReceiptV1, ContextScoutSessionEvidenceV1,
    ContextScoutSessionWindowV1, IntentDecisionSourceV1,
};
use tracedecay_contracts::{
    AuthorityReceipt, CoverageCompleteness, OmissionReason, ResolvedScope,
    RetrieverContributionState,
};
use tracedecay_domain::feedback::{FeedbackContentIdentityV1, FeedbackScopeV1};
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProviderId, RetrievalAnchorId, SessionId, UtcMicros,
    canonical_sha256,
};

use super::{ContextScoutErrorV1, MAX_SCOUT_EVIDENCE};

const EVIDENCE_CLAIM_DIGEST_DOMAIN: &str = "tracedecay.context-scout.evidence-claim.v1";
const SESSION_EVIDENCE_CLAIM_DIGEST_DOMAIN: &str =
    "tracedecay.context-scout.session-evidence-claim.v1";

trait ContextScoutRedactionReceiptExt {
    fn validate(&self, authority: &AuthorityReceipt) -> Result<(), ContextScoutErrorV1>;
}

impl ContextScoutRedactionReceiptExt for ContextScoutRedactionReceiptV1 {
    fn validate(&self, authority: &AuthorityReceipt) -> Result<(), ContextScoutErrorV1> {
        let disclosure = match self {
            Self::MetadataOnly { disclosure }
            | Self::Sanitized { disclosure, .. }
            | Self::Redacted { disclosure, .. } => *disclosure,
        };
        if disclosure != authority.disclosure {
            return Err(ContextScoutErrorV1::InvalidEvidence);
        }
        match self {
            Self::MetadataOnly { .. } => Ok(()),
            Self::Sanitized { receipts, .. } => {
                if receipts.is_empty()
                    || receipts.len() > MAX_SCOUT_EVIDENCE
                    || receipts
                        .windows(2)
                        .any(|pair| pair[0].as_str() >= pair[1].as_str())
                {
                    return Err(ContextScoutErrorV1::InvalidEvidence);
                }
                for receipt in receipts {
                    receipt
                        .validate()
                        .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
                }
                Ok(())
            }
            Self::Redacted { omissions, .. } => {
                if omissions.is_empty()
                    || omissions.len() > MAX_SCOUT_EVIDENCE
                    || omissions.iter().any(|omission| omission.count == 0)
                    || omissions.windows(2).any(|pair| {
                        (pair[0].domain, pair[0].reason) >= (pair[1].domain, pair[1].reason)
                    })
                    || !omissions
                        .iter()
                        .any(|omission| omission.reason == OmissionReason::Redacted)
                {
                    return Err(ContextScoutErrorV1::InvalidEvidence);
                }
                Ok(())
            }
        }
    }
}

trait ContextScoutEvidenceSourceReceiptExt {
    fn validate(&self) -> Result<(), ContextScoutErrorV1>;
}

impl ContextScoutEvidenceSourceReceiptExt for ContextScoutEvidenceSourceReceiptV1 {
    fn validate(&self) -> Result<(), ContextScoutErrorV1> {
        if self.temporal.requested_at.0 <= 0
            || self.temporal.resolved_at < self.temporal.requested_at
            || self
                .temporal
                .source_generation
                .as_ref()
                .is_some_and(|generation| generation.validate().is_err())
            || self
                .temporal
                .watermark_digest
                .as_ref()
                .is_some_and(|watermark| watermark.validate().is_err())
        {
            return Err(ContextScoutErrorV1::InvalidEvidence);
        }
        self.coverage
            .validate()
            .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        if self.anchors.len() > MAX_SCOUT_EVIDENCE
            || self
                .anchors
                .iter()
                .enumerate()
                .any(|(index, anchor)| self.anchors[index.saturating_add(1)..].contains(anchor))
        {
            return Err(ContextScoutErrorV1::InvalidEvidence);
        }
        for anchor in &self.anchors {
            anchor
                .validate()
                .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        }
        match self.contribution_state {
            RetrieverContributionState::Completed
                if self.coverage.completeness == CoverageCompleteness::Complete
                    && self.temporal.freshness == tracedecay_contracts::FreshnessState::Current
                    && !self.anchors.is_empty() =>
            {
                Ok(())
            }
            RetrieverContributionState::Partial
                if self.coverage.completeness == CoverageCompleteness::Partial
                    && self.temporal.freshness != tracedecay_contracts::FreshnessState::Stale
                    && !self.anchors.is_empty() =>
            {
                Ok(())
            }
            RetrieverContributionState::Stale
                if self.temporal.freshness == tracedecay_contracts::FreshnessState::Stale
                    && !self.anchors.is_empty() =>
            {
                Ok(())
            }
            RetrieverContributionState::Unavailable
            | RetrieverContributionState::Unsupported
            | RetrieverContributionState::Failed
            | RetrieverContributionState::Cancelled
            | RetrieverContributionState::TimedOut
                if self.temporal.freshness == tracedecay_contracts::FreshnessState::Unknown
                    && self.anchors.is_empty() =>
            {
                Ok(())
            }
            _ => Err(ContextScoutErrorV1::InvalidEvidence),
        }
    }
}

pub trait ContextScoutEvidenceEnvelopeExt {
    #[allow(clippy::too_many_arguments)]
    fn claim(
        scope: FeedbackScopeV1,
        authorized_scope: ResolvedScope,
        content: FeedbackContentIdentityV1,
        code_generation_id: CodeGenerationId,
        authority: AuthorityReceipt,
        redaction: ContextScoutRedactionReceiptV1,
        sources: Vec<ContextScoutEvidenceSourceReceiptV1>,
        claimed_at: UtcMicros,
    ) -> Result<ContextScoutEvidenceEnvelopeV1, ContextScoutErrorV1>;
    fn validate(&self) -> Result<(), ContextScoutErrorV1>;
    fn anchor_count(&self) -> usize;
    fn anchors(&self) -> impl Iterator<Item = &RetrievalAnchorId>;
}

impl ContextScoutEvidenceEnvelopeExt for ContextScoutEvidenceEnvelopeV1 {
    #[allow(clippy::too_many_arguments)]
    fn claim(
        scope: FeedbackScopeV1,
        authorized_scope: ResolvedScope,
        content: FeedbackContentIdentityV1,
        code_generation_id: CodeGenerationId,
        authority: AuthorityReceipt,
        mut redaction: ContextScoutRedactionReceiptV1,
        mut sources: Vec<ContextScoutEvidenceSourceReceiptV1>,
        claimed_at: UtcMicros,
    ) -> Result<Self, ContextScoutErrorV1> {
        canonicalize_redaction(&mut redaction);
        for source in &mut sources {
            source
                .anchors
                .sort_by(|left, right| left.as_str().cmp(right.as_str()));
        }
        sources.sort_by_key(|source| source.source);
        let availability = fold_availability(&sources)?;
        let claim_digest = canonical_sha256(&(
            EVIDENCE_CLAIM_DIGEST_DOMAIN,
            &scope,
            &authorized_scope,
            &content,
            &code_generation_id,
            &authority,
            &redaction,
            availability,
            &sources,
            claimed_at,
        ))
        .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        let envelope = Self {
            scope,
            authorized_scope,
            content,
            code_generation_id,
            authority,
            redaction,
            availability,
            sources,
            claimed_at,
            claim_digest,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    fn validate(&self) -> Result<(), ContextScoutErrorV1> {
        self.scope
            .validate()
            .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        self.authorized_scope
            .validate()
            .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        self.content
            .validate()
            .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        self.code_generation_id
            .validate()
            .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        if !matches!(self.content, FeedbackContentIdentityV1::SavedContent { .. })
            || self.claimed_at.0 <= 0
            || self.authority.revalidated_at > self.claimed_at
            || self.scope.project_id != self.authorized_scope.project_id
            || self.scope.repository_id != self.authorized_scope.repository_id
            || self.scope.worktree_id != self.authorized_scope.worktree_id
            || self
                .authorized_scope
                .reference
                .as_ref()
                .map(tracedecay_domain::RefId::as_str)
                != Some(self.scope.branch_ref.as_str())
            || self.sources.is_empty()
            || self.sources.len() > MAX_SCOUT_EVIDENCE
            || self
                .sources
                .windows(2)
                .any(|pair| pair[0].source >= pair[1].source)
        {
            return Err(ContextScoutErrorV1::InvalidEvidence);
        }
        self.authority
            .validate_for(&self.authorized_scope)
            .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        self.redaction.validate(&self.authority)?;
        for source in &self.sources {
            source.validate()?;
            if source.temporal.requested_mode != tracedecay_domain::TemporalModeV1::Current
                || source.temporal.source_generation.as_ref() != Some(&self.code_generation_id)
                || source.temporal.watermark_digest.is_none()
                || source.temporal.resolved_at > self.claimed_at
            {
                return Err(ContextScoutErrorV1::InvalidEvidence);
            }
        }
        if self.availability != fold_availability(&self.sources)? {
            return Err(ContextScoutErrorV1::InvalidEvidence);
        }
        let expected = canonical_sha256(&(
            EVIDENCE_CLAIM_DIGEST_DOMAIN,
            &self.scope,
            &self.authorized_scope,
            &self.content,
            &self.code_generation_id,
            &self.authority,
            &self.redaction,
            self.availability,
            &self.sources,
            self.claimed_at,
        ))
        .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        if expected != self.claim_digest {
            return Err(ContextScoutErrorV1::InvalidEvidence);
        }
        Ok(())
    }

    fn anchor_count(&self) -> usize {
        self.sources.iter().map(|source| source.anchors.len()).sum()
    }

    fn anchors(&self) -> impl Iterator<Item = &RetrievalAnchorId> {
        self.sources.iter().flat_map(|source| source.anchors.iter())
    }
}

/// Validation and anchors for either candidate evidence variant. Session
/// evidence cites no code anchors.
pub trait ContextScoutCandidateEvidenceExt {
    fn validate(&self) -> Result<(), ContextScoutErrorV1>;
    fn anchor_count(&self) -> usize;
    fn anchors(&self) -> impl Iterator<Item = &RetrievalAnchorId>;
}

impl ContextScoutCandidateEvidenceExt for ContextScoutCandidateEvidenceV1 {
    fn validate(&self) -> Result<(), ContextScoutErrorV1> {
        match self {
            Self::Code(evidence) => ContextScoutEvidenceEnvelopeExt::validate(evidence),
            Self::Session(evidence) => ContextScoutSessionEvidenceExt::validate(evidence),
        }
    }

    fn anchor_count(&self) -> usize {
        self.as_code()
            .map_or(0, ContextScoutEvidenceEnvelopeExt::anchor_count)
    }

    fn anchors(&self) -> impl Iterator<Item = &RetrievalAnchorId> {
        self.as_code()
            .into_iter()
            .flat_map(|evidence| evidence.sources.iter())
            .flat_map(|source| source.anchors.iter())
    }
}

/// Everything a session-evidence claim binds, before its digest exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextScoutSessionClaimInputV1 {
    pub authorized_scope: ResolvedScope,
    pub provider_id: ProviderId,
    pub session_id: SessionId,
    pub protected_session_id: [u8; 32],
    pub window: ContextScoutSessionWindowV1,
    pub prompt_excerpt_digest: Option<ManifestDigest>,
    pub code_generation_id: Option<CodeGenerationId>,
    pub authority: AuthorityReceipt,
    pub redaction: ContextScoutRedactionReceiptV1,
    pub availability: ContextScoutEvidenceAvailabilityV1,
    pub decision: ContextScoutIntentReceiptV1,
    pub claimed_at: UtcMicros,
}

pub trait ContextScoutSessionEvidenceExt: Sized {
    /// Canonicalizes (sorted receipts and facts), digests, and
    /// validates one session-evidence claim.
    fn claim(input: ContextScoutSessionClaimInputV1) -> Result<Self, ContextScoutErrorV1>;
    fn validate(&self) -> Result<(), ContextScoutErrorV1>;
}

impl ContextScoutSessionEvidenceExt for ContextScoutSessionEvidenceV1 {
    fn claim(input: ContextScoutSessionClaimInputV1) -> Result<Self, ContextScoutErrorV1> {
        let ContextScoutSessionClaimInputV1 {
            authorized_scope,
            provider_id,
            session_id,
            protected_session_id,
            window,
            prompt_excerpt_digest,
            code_generation_id,
            authority,
            mut redaction,
            availability,
            mut decision,
            claimed_at,
        } = input;
        canonicalize_redaction(&mut redaction);
        decision.facts.sort();
        let mut evidence = Self {
            authorized_scope,
            provider_id,
            session_id,
            protected_session_id,
            window,
            prompt_excerpt_digest,
            code_generation_id,
            authority,
            redaction,
            availability,
            decision,
            claimed_at,
            claim_digest: ManifestDigest::zero()
                .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?,
        };
        evidence.claim_digest = session_claim_digest(&evidence)?;
        ContextScoutSessionEvidenceExt::validate(&evidence)?;
        Ok(evidence)
    }

    fn validate(&self) -> Result<(), ContextScoutErrorV1> {
        self.authorized_scope
            .validate()
            .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        self.provider_id
            .validate()
            .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        self.session_id
            .validate()
            .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        self.window
            .window_digest
            .validate()
            .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        if let Some(digest) = &self.prompt_excerpt_digest {
            digest
                .validate()
                .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        }
        if let Some(generation) = &self.code_generation_id {
            generation
                .validate()
                .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        }
        if self.protected_session_id == [0; 32]
            || self.window.message_count == 0
            || self.window.first_store_id > self.window.last_store_id
            || self.claimed_at.0 <= 0
            || self.authority.revalidated_at > self.claimed_at
        {
            return Err(ContextScoutErrorV1::InvalidEvidence);
        }
        self.authority
            .validate_for(&self.authorized_scope)
            .map_err(|_| ContextScoutErrorV1::InvalidEvidence)?;
        self.redaction.validate(&self.authority)?;
        validate_intent_receipt(&self.decision)?;
        if session_claim_digest(self)? != self.claim_digest {
            return Err(ContextScoutErrorV1::InvalidEvidence);
        }
        Ok(())
    }
}

fn session_claim_digest(
    evidence: &ContextScoutSessionEvidenceV1,
) -> Result<ManifestDigest, ContextScoutErrorV1> {
    canonical_sha256(&(
        SESSION_EVIDENCE_CLAIM_DIGEST_DOMAIN,
        &evidence.authorized_scope,
        &evidence.provider_id,
        &evidence.session_id,
        evidence.protected_session_id,
        &evidence.window,
        &evidence.prompt_excerpt_digest,
        &evidence.code_generation_id,
        &evidence.authority,
        &evidence.redaction,
        evidence.availability,
        &evidence.decision,
        evidence.claimed_at,
    ))
    .map_err(|_| ContextScoutErrorV1::InvalidEvidence)
}

/// A receipt is canonical (facts strictly ordered), bounded, and names the
/// selected intent among its observed action facts.
fn validate_intent_receipt(
    receipt: &ContextScoutIntentReceiptV1,
) -> Result<(), ContextScoutErrorV1> {
    let max_intents = ContextScoutIntentKindV1::ALL.len();
    if receipt.facts.len() > max_intents
        || receipt.facts.len() > MAX_SCOUT_EVIDENCE
        || receipt.facts.windows(2).any(|pair| pair[0] >= pair[1])
        || !receipt.facts.contains(&receipt.selected)
        || receipt.source != IntentDecisionSourceV1::ActionFact
    {
        return Err(ContextScoutErrorV1::InvalidEvidence);
    }
    Ok(())
}

fn canonicalize_redaction(redaction: &mut ContextScoutRedactionReceiptV1) {
    match redaction {
        ContextScoutRedactionReceiptV1::MetadataOnly { .. } => {}
        ContextScoutRedactionReceiptV1::Sanitized { receipts, .. } => {
            receipts.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        }
        ContextScoutRedactionReceiptV1::Redacted { omissions, .. } => {
            omissions.sort_by_key(|omission| (omission.domain, omission.reason));
        }
    }
}

fn fold_availability(
    sources: &[ContextScoutEvidenceSourceReceiptV1],
) -> Result<ContextScoutEvidenceAvailabilityV1, ContextScoutErrorV1> {
    if sources.is_empty() {
        return Err(ContextScoutErrorV1::InvalidEvidence);
    }
    for source in sources {
        source.validate()?;
    }
    if sources
        .iter()
        .all(|source| source.contribution_state == RetrieverContributionState::Completed)
    {
        return Ok(ContextScoutEvidenceAvailabilityV1::Complete);
    }
    if sources
        .iter()
        .any(|source| source.contribution_state == RetrieverContributionState::Cancelled)
    {
        return Ok(ContextScoutEvidenceAvailabilityV1::Cancelled);
    }
    if sources
        .iter()
        .any(|source| source.contribution_state == RetrieverContributionState::Stale)
    {
        return Ok(ContextScoutEvidenceAvailabilityV1::Stale);
    }
    if sources.iter().any(|source| {
        matches!(
            source.contribution_state,
            RetrieverContributionState::Completed | RetrieverContributionState::Partial
        )
    }) {
        return Ok(ContextScoutEvidenceAvailabilityV1::Partial);
    }
    Ok(ContextScoutEvidenceAvailabilityV1::Unavailable)
}

#[cfg(test)]
pub(super) fn fixture_context_scout_evidence() -> ContextScoutEvidenceEnvelopeV1 {
    use tracedecay_contracts::{
        CoverageDomainState, DisclosureClass, EvidenceCoverage, EvidenceDomain, FreshnessState,
        PolicyDecisionRef, TemporalState,
    };
    use tracedecay_domain::{
        CommitId, ComponentVersion, ProjectId, RefId, RepositoryId, TemporalModeV1, WorktreeId,
    };

    use tracedecay_domain::test_fixtures::digest;
    use tracedecay_domain::test_fixtures::id;

    let authorized_scope = ResolvedScope::new(
        id::<ProjectId>("project.scout"),
        id::<RepositoryId>("repository.scout"),
        id::<WorktreeId>("worktree.scout"),
        Some(id::<RefId>("refs/heads/main")),
    )
    .unwrap();
    ContextScoutEvidenceEnvelopeV1::claim(
        FeedbackScopeV1 {
            project_id: id("project.scout"),
            repository_id: id("repository.scout"),
            worktree_id: id("worktree.scout"),
            branch_ref: "refs/heads/main".to_owned(),
            head_commit_id: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        },
        authorized_scope.clone(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('c'),
            file_digest: digest('d'),
        },
        id::<CodeGenerationId>("generation.scout"),
        AuthorityReceipt {
            grant_id: id("grant.scout"),
            grant_revision: 1,
            grant_digest: digest('a'),
            authorized_scope_digest: authorized_scope.scope_digest.clone(),
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.scout",
                1,
                digest('b'),
                ComponentVersion::new("policy.scout.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: UtcMicros(1),
        },
        ContextScoutRedactionReceiptV1::MetadataOnly {
            disclosure: DisclosureClass::Evidence,
        },
        vec![ContextScoutEvidenceSourceReceiptV1 {
            source: ContextScoutEvidenceSourceKindV1::Query,
            contribution_state: RetrieverContributionState::Completed,
            temporal: TemporalState {
                requested_mode: TemporalModeV1::Current,
                requested_at: UtcMicros(1),
                resolved_at: UtcMicros(2),
                source_generation: Some(id("generation.scout")),
                code_graph_freshness: None,
                watermark_digest: Some(digest('e')),
                freshness: FreshnessState::Current,
            },
            coverage: EvidenceCoverage {
                requested_domains: vec![EvidenceDomain::Anchor],
                visited: Some(1),
                eligible: Some(1),
                returned: 1,
                completeness: CoverageCompleteness::Complete,
                domains: vec![CoverageDomainState {
                    domain: EvidenceDomain::Anchor,
                    completeness: CoverageCompleteness::Complete,
                }],
            },
            anchors: vec![id("anchor.scout")],
        }],
        UtcMicros(2),
    )
    .unwrap()
}

#[cfg(test)]
pub(super) fn fixture_context_scout_session_claim() -> ContextScoutSessionClaimInputV1 {
    let code = fixture_context_scout_evidence();
    ContextScoutSessionClaimInputV1 {
        authorized_scope: code.authorized_scope,
        provider_id: tracedecay_domain::test_fixtures::id("provider.claude"),
        session_id: tracedecay_domain::test_fixtures::id("session.scout"),
        protected_session_id: [3; 32],
        window: ContextScoutSessionWindowV1 {
            first_store_id: 40,
            last_store_id: 52,
            message_count: 6,
            window_digest: tracedecay_domain::test_fixtures::digest('f'),
            newest_timestamp: Some(1_700_000_000_000),
        },
        prompt_excerpt_digest: Some(tracedecay_domain::test_fixtures::digest('9')),
        code_generation_id: Some(code.code_generation_id),
        authority: code.authority,
        redaction: code.redaction,
        availability: ContextScoutEvidenceAvailabilityV1::Complete,
        decision: ContextScoutIntentReceiptV1 {
            facts: vec![
                ContextScoutIntentKindV1::Search,
                ContextScoutIntentKindV1::SessionRecall,
            ],
            selected: ContextScoutIntentKindV1::SessionRecall,
            source: IntentDecisionSourceV1::ActionFact,
        },
        claimed_at: UtcMicros(2),
    }
}

#[cfg(test)]
pub(super) fn fixture_context_scout_session_evidence() -> ContextScoutSessionEvidenceV1 {
    ContextScoutSessionEvidenceV1::claim(fixture_context_scout_session_claim()).unwrap()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn rejected(mutate: impl FnOnce(&mut ContextScoutSessionClaimInputV1)) {
        let mut claim = fixture_context_scout_session_claim();
        mutate(&mut claim);
        assert_eq!(
            ContextScoutSessionEvidenceV1::claim(claim),
            Err(ContextScoutErrorV1::InvalidEvidence)
        );
    }

    #[test]
    fn session_claim_canonicalizes_and_validates() {
        let evidence = fixture_context_scout_session_evidence();
        assert_eq!(
            evidence.decision.facts,
            vec![
                ContextScoutIntentKindV1::Search,
                ContextScoutIntentKindV1::SessionRecall
            ]
        );
        ContextScoutSessionEvidenceExt::validate(&evidence).unwrap();
        assert_eq!(
            ContextScoutSessionEvidenceV1::claim(fixture_context_scout_session_claim()).unwrap(),
            evidence,
            "claims are deterministic"
        );
    }

    #[test]
    fn session_claim_digest_binds_every_field() {
        let evidence = fixture_context_scout_session_evidence();
        let mut tampered = evidence.clone();
        tampered.window.last_store_id += 1;
        assert_eq!(
            ContextScoutSessionEvidenceExt::validate(&tampered),
            Err(ContextScoutErrorV1::InvalidEvidence)
        );
        let mut tampered = evidence.clone();
        tampered.decision.facts[0] = ContextScoutIntentKindV1::FileRead;
        assert_eq!(
            ContextScoutSessionEvidenceExt::validate(&tampered),
            Err(ContextScoutErrorV1::InvalidEvidence)
        );
        let mut tampered = evidence;
        tampered.claim_digest = tracedecay_domain::test_fixtures::digest('7');
        assert_eq!(
            ContextScoutSessionEvidenceExt::validate(&tampered),
            Err(ContextScoutErrorV1::InvalidEvidence)
        );
    }

    #[test]
    fn session_claim_requires_matching_scope_and_ordered_authority() {
        rejected(|claim| {
            claim.authority.authorized_scope_digest = tracedecay_domain::test_fixtures::digest('6');
        });
        rejected(|claim| claim.claimed_at = UtcMicros(0));
        rejected(|claim| claim.authority.revalidated_at = UtcMicros(3));
        rejected(|claim| claim.protected_session_id = [0; 32]);
        rejected(|claim| {
            claim.redaction = ContextScoutRedactionReceiptV1::MetadataOnly {
                disclosure: tracedecay_contracts::DisclosureClass::Metadata,
            };
        });
    }

    #[test]
    fn session_window_must_be_non_empty_and_ordered() {
        rejected(|claim| claim.window.message_count = 0);
        rejected(|claim| {
            claim.window.first_store_id = 53;
        });
        let mut single = fixture_context_scout_session_claim();
        single.window.first_store_id = 52;
        single.window.message_count = 1;
        ContextScoutSessionEvidenceV1::claim(single).unwrap();
    }

    #[test]
    fn intent_receipt_must_name_an_observed_action_fact() {
        // The selected intent must be one of the observed facts.
        rejected(|claim| {
            claim.decision.selected = ContextScoutIntentKindV1::Impact;
        });
        // Repeated facts are not canonical after claim sorting.
        rejected(|claim| {
            claim.decision.facts.push(ContextScoutIntentKindV1::Search);
        });
        let mut unsorted = fixture_context_scout_session_claim();
        unsorted.decision.facts.reverse();
        let canonical = ContextScoutSessionEvidenceV1::claim(unsorted).unwrap();
        assert_eq!(
            canonical.decision.facts,
            vec![
                ContextScoutIntentKindV1::Search,
                ContextScoutIntentKindV1::SessionRecall,
            ]
        );
        // A fact-only pass is valid with only observed actions.
        let mut fact_only = fixture_context_scout_session_claim();
        fact_only.decision.facts = vec![ContextScoutIntentKindV1::Search];
        fact_only.decision.selected = ContextScoutIntentKindV1::Search;
        fact_only.decision.source = IntentDecisionSourceV1::ActionFact;
        ContextScoutSessionEvidenceV1::claim(fact_only).unwrap();
    }

    #[test]
    fn candidate_evidence_union_is_tagged_and_dispatches() {
        let session: ContextScoutCandidateEvidenceV1 =
            fixture_context_scout_session_evidence().into();
        let code: ContextScoutCandidateEvidenceV1 = fixture_context_scout_evidence().into();
        ContextScoutCandidateEvidenceExt::validate(&session).unwrap();
        ContextScoutCandidateEvidenceExt::validate(&code).unwrap();
        assert_eq!(session.anchor_count(), 0);
        assert_eq!(session.anchors().count(), 0);
        assert_eq!(code.anchor_count(), 1);

        let encoded = serde_json::to_value(&session).unwrap();
        assert_eq!(encoded["kind"], "session");
        assert_eq!(
            serde_json::from_value::<ContextScoutCandidateEvidenceV1>(encoded).unwrap(),
            session
        );
        let encoded = serde_json::to_value(&code).unwrap();
        assert_eq!(encoded["kind"], "code");
        assert_eq!(
            serde_json::from_value::<ContextScoutCandidateEvidenceV1>(encoded).unwrap(),
            code
        );
        // The pre-union shape (a bare code envelope) does not decode.
        let bare = serde_json::to_value(fixture_context_scout_evidence()).unwrap();
        assert!(serde_json::from_value::<ContextScoutCandidateEvidenceV1>(bare).is_err());
    }
}
