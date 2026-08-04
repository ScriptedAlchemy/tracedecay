//! Transport-neutral composition of authenticated remote query results.
//!
//! This module never exposes locators, database bytes, credentials, or SQL.
//! Each claim is independent so a valid digest cannot imply authorization,
//! freshness, completeness, or shard coverage.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    error::ApplicationContractError,
    result::{ApplicationProblem, AuthorityReceipt, LegalAction, RetryDirective, SafeDiagnostic},
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityClaimV1 {
    Verified,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticityClaimV1 {
    Authenticated,
    Rejected,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteFreshnessV1 {
    Current,
    Stale,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCompletenessV1 {
    Complete,
    Partial,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationClaimV1 {
    Authorized,
    Denied,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShardCoverageStateV1 {
    Complete,
    Stale,
    Partial,
    Unknown,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryManifestBindingV1 {
    pub brain_id: String,
    pub shard_id: String,
    pub generation_id: String,
    pub schema_digest: [u8; 32],
    pub watermark_sequence: u64,
    pub placement_revision: u64,
    pub authority_epoch: u64,
    pub cache_age_millis: u64,
    pub cache_lag_commits: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ExpectedRemoteShardV1 {
    pub brain_id: String,
    pub shard_id: String,
    pub generation_id: String,
}

impl From<&QueryManifestBindingV1> for ExpectedRemoteShardV1 {
    fn from(manifest: &QueryManifestBindingV1) -> Self {
        Self {
            brain_id: manifest.brain_id.clone(),
            shard_id: manifest.shard_id.clone(),
            generation_id: manifest.generation_id.clone(),
        }
    }
}

impl QueryManifestBindingV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        for (field, value) in [
            ("remote brain id", self.brain_id.as_str()),
            ("remote shard id", self.shard_id.as_str()),
            ("remote generation id", self.generation_id.as_str()),
        ] {
            if value.is_empty()
                || value.len() > 512
                || value.trim() != value
                || value.chars().any(char::is_control)
            {
                return Err(ApplicationContractError::InvalidIdentifier { field });
            }
        }
        if self.schema_digest == [0; 32] {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote schema digest",
            });
        }
        for (field, value) in [
            ("remote watermark sequence", self.watermark_sequence),
            ("remote placement revision", self.placement_revision),
            ("remote authority epoch", self.authority_epoch),
        ] {
            if value == 0 {
                return Err(ApplicationContractError::ZeroValue { field });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PendingLocalObservationsV1 {
    pub count: u64,
    pub oldest_age_millis: Option<u64>,
    pub has_sequence_gap: bool,
    pub has_quarantined: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingLocalUnavailableReasonV1 {
    RequestingNodeSpoolNotSupplied,
    AuthorityUnavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
pub enum PendingLocalEvidenceV1 {
    Available {
        evidence: PendingLocalObservationsV1,
    },
    Unavailable {
        reason: PendingLocalUnavailableReasonV1,
    },
}

impl From<PendingLocalObservationsV1> for PendingLocalEvidenceV1 {
    fn from(evidence: PendingLocalObservationsV1) -> Self {
        Self::Available { evidence }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ShardQueryContributionV1<T> {
    pub manifest: QueryManifestBindingV1,
    pub integrity: IntegrityClaimV1,
    pub authenticity: AuthenticityClaimV1,
    pub freshness: RemoteFreshnessV1,
    pub completeness: RemoteCompletenessV1,
    pub authorization: AuthorizationClaimV1,
    pub coverage: ShardCoverageStateV1,
    pub authority_receipt: Option<AuthorityReceipt>,
    pub value: Option<T>,
    pub reason_code: Option<String>,
}

impl<T> ShardQueryContributionV1<T> {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.manifest.validate()?;
        let may_disclose = self.integrity == IntegrityClaimV1::Verified
            && self.authenticity == AuthenticityClaimV1::Authenticated
            && self.authorization == AuthorizationClaimV1::Authorized
            && self.authority_receipt.is_some();
        if self.value.is_some() && !may_disclose {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote query disclosure",
            });
        }
        if self.coverage == ShardCoverageStateV1::Complete
            && (self.freshness != RemoteFreshnessV1::Current
                || self.completeness != RemoteCompletenessV1::Complete
                || !may_disclose
                || self.value.is_none())
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote complete coverage",
            });
        }
        if matches!(
            self.coverage,
            ShardCoverageStateV1::Stale
                | ShardCoverageStateV1::Partial
                | ShardCoverageStateV1::Unknown
                | ShardCoverageStateV1::Unavailable
        ) && self.reason_code.is_none()
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote degraded coverage reason",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteQueryCompositionV1<T> {
    pub contributions: Vec<ShardQueryContributionV1<T>>,
    pub pending_local: PendingLocalEvidenceV1,
    pub coverage: ShardCoverageStateV1,
}

impl<T> RemoteQueryCompositionV1<T> {
    pub fn compose<P>(
        expected_shards: BTreeSet<ExpectedRemoteShardV1>,
        contributions: Vec<ShardQueryContributionV1<T>>,
        pending_local: P,
        maximum_current_cache_age_millis: u64,
    ) -> Result<Self, ApplicationProblem>
    where
        P: Into<PendingLocalEvidenceV1>,
    {
        let pending_local = pending_local.into();
        if expected_shards.is_empty()
            || contributions.is_empty()
            || maximum_current_cache_age_millis == 0
        {
            return Err(remote_unavailable(
                "remote_query_authority_unavailable",
                "Remote query authority is unavailable.",
            ));
        }
        let mut actual_shards = BTreeSet::new();
        for contribution in &contributions {
            contribution.validate().map_err(|_| {
                remote_unavailable(
                    "remote_query_manifest_invalid",
                    "Remote query material could not be verified.",
                )
            })?;
            if contribution.freshness == RemoteFreshnessV1::Current
                && (contribution.manifest.cache_lag_commits != 0
                    || contribution.manifest.cache_age_millis > maximum_current_cache_age_millis)
            {
                return Err(remote_unavailable(
                    "remote_query_freshness_invalid",
                    "Remote query freshness could not be verified.",
                ));
            }
            if !actual_shards.insert(ExpectedRemoteShardV1::from(&contribution.manifest)) {
                return Err(remote_unavailable(
                    "remote_query_shard_duplicate",
                    "Remote query shard inventory contains a duplicate.",
                ));
            }
        }
        if actual_shards != expected_shards {
            return Err(remote_unavailable(
                "remote_query_shard_inventory_mismatch",
                "Remote query shard inventory is incomplete.",
            ));
        }
        let coverage = aggregate_coverage(&contributions, &pending_local);
        Ok(Self {
            contributions,
            pending_local,
            coverage,
        })
    }

    pub fn is_complete(&self) -> bool {
        self.coverage == ShardCoverageStateV1::Complete
    }
}

fn aggregate_coverage<T>(
    contributions: &[ShardQueryContributionV1<T>],
    pending: &PendingLocalEvidenceV1,
) -> ShardCoverageStateV1 {
    if contributions
        .iter()
        .any(|item| item.coverage == ShardCoverageStateV1::Unavailable)
    {
        return ShardCoverageStateV1::Unavailable;
    }
    if contributions
        .iter()
        .any(|item| item.coverage == ShardCoverageStateV1::Unknown)
    {
        return ShardCoverageStateV1::Unknown;
    }
    let available_pending = match pending {
        PendingLocalEvidenceV1::Available { evidence } => Some(evidence),
        PendingLocalEvidenceV1::Unavailable { .. } => None,
    };
    if available_pending.is_some_and(|pending| pending.has_sequence_gap || pending.has_quarantined)
        || contributions
            .iter()
            .any(|item| item.coverage == ShardCoverageStateV1::Partial)
    {
        return ShardCoverageStateV1::Partial;
    }
    if available_pending.is_some_and(|pending| pending.count > 0)
        || contributions
            .iter()
            .any(|item| item.coverage == ShardCoverageStateV1::Stale)
    {
        return ShardCoverageStateV1::Stale;
    }
    ShardCoverageStateV1::Complete
}

fn remote_unavailable(code: &str, message: &str) -> ApplicationProblem {
    ApplicationProblem::Unavailable {
        diagnostic: SafeDiagnostic::new(code, message)
            .expect("static remote problem diagnostic is valid"),
        retry: RetryDirective::AfterRevalidate,
        legal_actions: vec![LegalAction::Refresh, LegalAction::Reconcile],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> QueryManifestBindingV1 {
        QueryManifestBindingV1 {
            brain_id: "brain.remote".into(),
            shard_id: "shard.project".into(),
            generation_id: "generation.7".into(),
            schema_digest: [1; 32],
            watermark_sequence: 9,
            placement_revision: 3,
            authority_epoch: 4,
            cache_age_millis: 10,
            cache_lag_commits: 0,
        }
    }

    fn expected_shards() -> BTreeSet<ExpectedRemoteShardV1> {
        BTreeSet::from([ExpectedRemoteShardV1::from(&manifest())])
    }

    #[test]
    fn absent_authority_is_unavailable_not_empty_success() {
        let result = RemoteQueryCompositionV1::<String>::compose(
            expected_shards(),
            Vec::new(),
            PendingLocalObservationsV1 {
                count: 0,
                oldest_age_millis: None,
                has_sequence_gap: false,
                has_quarantined: false,
            },
            100,
        );
        assert!(matches!(
            result,
            Err(ApplicationProblem::Unavailable { .. })
        ));
    }

    #[test]
    fn pending_local_observations_prevent_complete_coverage() {
        let contribution = ShardQueryContributionV1 {
            manifest: manifest(),
            integrity: IntegrityClaimV1::Verified,
            authenticity: AuthenticityClaimV1::Authenticated,
            freshness: RemoteFreshnessV1::Current,
            completeness: RemoteCompletenessV1::Complete,
            authorization: AuthorizationClaimV1::Authorized,
            authority_receipt: None,
            coverage: ShardCoverageStateV1::Partial,
            value: None::<String>,
            reason_code: Some("authorization_receipt_unavailable".into()),
        };
        let result = RemoteQueryCompositionV1::compose(
            expected_shards(),
            vec![contribution],
            PendingLocalObservationsV1 {
                count: 2,
                oldest_age_millis: Some(50),
                has_sequence_gap: false,
                has_quarantined: false,
            },
            100,
        )
        .unwrap();
        assert_eq!(result.coverage, ShardCoverageStateV1::Partial);
    }

    #[test]
    fn unverifiable_material_cannot_disclose_a_value() {
        let contribution = ShardQueryContributionV1 {
            manifest: manifest(),
            integrity: IntegrityClaimV1::Unknown,
            authenticity: AuthenticityClaimV1::Authenticated,
            freshness: RemoteFreshnessV1::Stale,
            completeness: RemoteCompletenessV1::Partial,
            authorization: AuthorizationClaimV1::Authorized,
            authority_receipt: None,
            coverage: ShardCoverageStateV1::Stale,
            value: Some("must-not-leak".to_owned()),
            reason_code: Some("integrity_unknown".into()),
        };
        assert!(contribution.validate().is_err());
    }

    #[test]
    fn duplicate_or_missing_shards_cannot_claim_complete_coverage() {
        let contribution = ShardQueryContributionV1 {
            manifest: manifest(),
            integrity: IntegrityClaimV1::Unknown,
            authenticity: AuthenticityClaimV1::Unknown,
            freshness: RemoteFreshnessV1::Unknown,
            completeness: RemoteCompletenessV1::Unknown,
            authorization: AuthorizationClaimV1::Unknown,
            authority_receipt: None,
            coverage: ShardCoverageStateV1::Unavailable,
            value: None::<String>,
            reason_code: Some("authority_unavailable".into()),
        };
        let pending = PendingLocalObservationsV1 {
            count: 0,
            oldest_age_millis: None,
            has_sequence_gap: false,
            has_quarantined: false,
        };
        assert!(
            RemoteQueryCompositionV1::compose(
                expected_shards(),
                vec![contribution.clone(), contribution],
                pending,
                100,
            )
            .is_err()
        );
    }
}
