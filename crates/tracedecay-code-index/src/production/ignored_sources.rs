use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ManifestDigest, ProjectionKeyV1, RepositoryDirtyStateV1, SanitizedCodeSnapshotV1,
    SnapshotFileDispositionV1, TreeId, UtcMicros, canonical_sha256, validate_code_logical_path,
};

use super::{CodeIndexCapturedFileV1, CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1};
use crate::generations::RebuildTriggerV1;

const IGNORED_SOURCE_ROSTER_DIGEST_DOMAIN: &str = "tracedecay.code-index.ignored-source-roster.v1";

pub const MAX_IGNORED_DEPENDENCY_ENTRYPOINT_BYTES_V1: usize =
    crate::extract::MAX_EXTRACTION_SOURCE_BYTES;

/// One sanitized source path that a dirty-worktree capture explicitly admitted
/// despite ordinary ignore policy.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexIgnoredSourceAdmissionV1 {
    pub logical_path: String,
}

/// Inputs for one complete immutable code-index generation.
#[derive(Clone, Debug)]
pub struct CodeIndexBuildRequestV1 {
    pub snapshot: SanitizedCodeSnapshotV1,
    pub captured_files: Vec<CodeIndexCapturedFileV1>,
    /// Capture-reported paths are evidence only; digest equality remains the
    /// sole reuse authority.
    pub changed_files: BTreeSet<String>,
    /// Additional conservative invalidations that the application boundary,
    /// rather than the sanitized snapshot, is authoritative to report.
    pub invalidations: BTreeSet<RebuildTriggerV1>,
    /// Exact Git tree and dirty-state evidence paired with the snapshot's
    /// repository/worktree/ref/commit identity. Missing tree is truthful for
    /// unborn or unavailable Git state and never replaced with a digest.
    pub repository_parse_identity: CodeIndexRepositoryParseIdentityV1,
    /// Canonical paths admitted from ignored source roots for this exact dirty
    /// snapshot. The roster is required even when empty.
    pub ignored_source_admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
    pub sealed_at: UtcMicros,
    pub target_projection_key: ProjectionKeyV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexRepositoryParseIdentityV1 {
    pub tree: Option<TreeId>,
    pub dirty: RepositoryDirtyStateV1,
}

#[derive(Serialize)]
struct IgnoredSourceRosterDigestInputV1<'a> {
    domain: &'static str,
    admissions: &'a [CodeIndexIgnoredSourceAdmissionV1],
}

#[derive(Clone, Debug)]
pub(super) struct IgnoredSourceRosterV1 {
    admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
    digest: ManifestDigest,
}

impl IgnoredSourceRosterV1 {
    pub(super) fn admit(
        snapshot: &SanitizedCodeSnapshotV1,
        repository_parse_identity: &CodeIndexRepositoryParseIdentityV1,
        admissions: &[CodeIndexIgnoredSourceAdmissionV1],
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let admissions = admissions.to_vec();
        let digest = Self::compute_digest(&admissions)?;
        let roster = Self { admissions, digest };
        roster.validate(snapshot, repository_parse_identity)?;
        Ok(roster)
    }

    pub(super) fn restore(
        snapshot: &SanitizedCodeSnapshotV1,
        repository_parse_identity: &CodeIndexRepositoryParseIdentityV1,
        admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
        digest: ManifestDigest,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let roster = Self { admissions, digest };
        roster.validate(snapshot, repository_parse_identity)?;
        Ok(roster)
    }

    pub(super) fn validate(
        &self,
        snapshot: &SanitizedCodeSnapshotV1,
        repository_parse_identity: &CodeIndexRepositoryParseIdentityV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        if !self.admissions.is_empty()
            && (repository_parse_identity.dirty != RepositoryDirtyStateV1::Dirty
                || snapshot.source_revision.is_some())
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "ignored-source admissions require an unpinned dirty repository snapshot"
                    .to_owned(),
            ));
        }
        Self::validate_membership(snapshot, &self.admissions)?;
        if Self::compute_digest(&self.admissions)? != self.digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "ignored-source roster digest does not match its admissions".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) fn admissions(&self) -> &[CodeIndexIgnoredSourceAdmissionV1] {
        &self.admissions
    }

    pub(super) fn digest(&self) -> &ManifestDigest {
        &self.digest
    }

    fn validate_membership(
        snapshot: &SanitizedCodeSnapshotV1,
        admissions: &[CodeIndexIgnoredSourceAdmissionV1],
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let mut previous_path: Option<&str> = None;
        for admission in admissions {
            validate_code_logical_path(&admission.logical_path)
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
            if previous_path.is_some_and(|previous| previous >= admission.logical_path.as_str()) {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "ignored-source admissions must be ordered and unique".to_owned(),
                ));
            }
            previous_path = Some(&admission.logical_path);

            let mut matches = snapshot
                .files
                .iter()
                .filter(|file| file.logical_path == admission.logical_path);
            let Some(file) = matches.next() else {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "ignored-source admission is absent from the snapshot".to_owned(),
                ));
            };
            if file.disposition != SnapshotFileDispositionV1::Present || matches.next().is_some() {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "ignored-source admission must identify exactly one present snapshot file"
                        .to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn compute_digest(
        admissions: &[CodeIndexIgnoredSourceAdmissionV1],
    ) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
        canonical_sha256(&IgnoredSourceRosterDigestInputV1 {
            domain: IGNORED_SOURCE_ROSTER_DIGEST_DOMAIN,
            admissions,
        })
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
    }
}

impl CodeIndexPublishedGenerationV1 {
    pub fn repository_parse_identity(&self) -> &CodeIndexRepositoryParseIdentityV1 {
        &self.repository_parse_identity
    }

    pub fn ignored_source_admissions(&self) -> &[CodeIndexIgnoredSourceAdmissionV1] {
        self.ignored_source_roster.admissions()
    }

    pub fn ignored_source_admissions_digest(&self) -> &ManifestDigest {
        self.ignored_source_roster.digest()
    }
}
