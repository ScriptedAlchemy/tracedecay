#[cfg(test)]
use tracedecay_application::context_scout::ContextScoutEvidenceSourceKindV1;
use tracedecay_application::context_scout::{
    ContextScoutEvidenceAvailabilityV1, ContextScoutEvidenceEnvelopeV1,
    ContextScoutEvidenceSourceReceiptV1, ContextScoutRedactionReceiptV1,
};
use tracedecay_application::{
    AuthorityReceipt, CoverageCompleteness, OmissionReason, ResolvedScope,
    RetrieverContributionState,
};
use tracedecay_domain::feedback::{FeedbackContentIdentityV1, FeedbackScopeV1};
use tracedecay_domain::{CodeGenerationId, RetrievalAnchorId, UtcMicros, canonical_sha256};

use super::{ContextScoutErrorV1, MAX_SCOUT_EVIDENCE};

const EVIDENCE_CLAIM_DIGEST_DOMAIN: &str = "tracedecay.context-scout.evidence-claim.v1";

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
                    && self.temporal.freshness
                        == tracedecay_application::FreshnessState::Current
                    && !self.anchors.is_empty() =>
            {
                Ok(())
            }
            RetrieverContributionState::Partial
                if self.coverage.completeness == CoverageCompleteness::Partial
                    && self.temporal.freshness != tracedecay_application::FreshnessState::Stale
                    && !self.anchors.is_empty() =>
            {
                Ok(())
            }
            RetrieverContributionState::Stale
                if self.temporal.freshness == tracedecay_application::FreshnessState::Stale
                    && !self.anchors.is_empty() =>
            {
                Ok(())
            }
            RetrieverContributionState::Unavailable
            | RetrieverContributionState::Unsupported
            | RetrieverContributionState::Failed
            | RetrieverContributionState::Cancelled
            | RetrieverContributionState::TimedOut
                if self.temporal.freshness == tracedecay_application::FreshnessState::Unknown
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
        match &mut redaction {
            ContextScoutRedactionReceiptV1::MetadataOnly { .. } => {}
            ContextScoutRedactionReceiptV1::Sanitized { receipts, .. } => {
                receipts.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            }
            ContextScoutRedactionReceiptV1::Redacted { omissions, .. } => {
                omissions.sort_by_key(|omission| (omission.domain, omission.reason));
            }
        }
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
    use tracedecay_application::{
        CoverageDomainState, DisclosureClass, EvidenceCoverage, EvidenceDomain, FreshnessState,
        PolicyDecisionRef, TemporalState,
    };
    use tracedecay_domain::{
        CommitId, ComponentVersion, ManifestDigest, ProjectId, RefId, RepositoryId, TemporalModeV1,
        WorktreeId,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }
    fn digest(character: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", character.to_string().repeat(64))).unwrap()
    }

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
