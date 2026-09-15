use std::collections::HashMap;
use std::ops::Range;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub use tracedecay_code_extraction::{
    CloneBodyEligibilityV1, CloneBodyRenameStatusV1, ConservativeCloneTokenV1,
};
use tracedecay_code_extraction::{
    CloneBodyRenameIssueV1, CloneBodyTokenizationIssueV1, CloneBodyTokenizationStatusV1,
    ExtractedCloneBodyV1,
};
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, SourceSpan, SymbolOccurrenceId,
    WorktreeId, canonical_json_bytes, canonical_sha256,
};

const BODY_DIGEST_DOMAIN: &str = "tracedecay.clone-body.v1";
const CONSERVATIVE_DIGEST_DOMAIN: &str = "tracedecay.clone-conservative.v1";
const RENAME_DIGEST_DOMAIN: &str = "tracedecay.clone-rename.v1";
const PAYLOAD_DIGEST_DOMAIN: &str = "tracedecay.clone-payload.v1";
const FINGERPRINT_DOMAIN: &str = "tracedecay.clone-fingerprint.v1";

pub const CLONE_FINGERPRINT_K_V1: usize = 7;
pub const CLONE_FINGERPRINT_WINDOW_V1: usize = 8;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct CloneFingerprintPositionV1 {
    pub fingerprint: u64,
    pub token_position: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CloneTokenAnchorV1 {
    pub fingerprint: u64,
    pub left_token_position: u32,
    pub right_token_position: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloneTokenSpanV1 {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneAlignedDifferenceV1 {
    pub left_span: CloneTokenSpanV1,
    pub right_span: CloneTokenSpanV1,
    pub left_tokens: Vec<ConservativeCloneTokenV1>,
    pub right_tokens: Vec<ConservativeCloneTokenV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneWholeBodyAlignmentV1 {
    pub ordered_anchors: Vec<CloneTokenAnchorV1>,
    pub shared_token_count: u32,
    pub left_coverage_millionths: u32,
    pub right_coverage_millionths: u32,
    pub differences: Vec<CloneAlignedDifferenceV1>,
    pub work: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloneAlignmentStopReasonV1 {
    WorkBudgetExhausted,
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloneAlignmentStoppedV1 {
    pub reason: CloneAlignmentStopReasonV1,
    pub work: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum CloneNormalizationClassV1 {
    Conservative = 1,
    Rename = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloneFingerprintStreamV1<'a> {
    pub class: CloneNormalizationClassV1,
    pub normalization_revision: u16,
    pub tokens: &'a [ConservativeCloneTokenV1],
    pub rename_tier_unavailable: Option<CloneBodyRenameStatusV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneSelectedBlockV1 {
    source_payload_digest: ManifestDigest,
    source_body_digest: ManifestDigest,
    language: String,
    class: CloneNormalizationClassV1,
    normalization_revision: u16,
    rename_tier_unavailable: Option<CloneBodyRenameStatusV1>,
    tokens: Vec<ConservativeCloneTokenV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct CloneExactKeyV1 {
    pub class: CloneNormalizationClassV1,
    pub normalization_revision: u16,
    pub digest: ManifestDigest,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CloneBodyPayloadV1 {
    pub payload_digest: ManifestDigest,
    pub language: String,
    pub symbol_kind: String,
    pub body_digest: ManifestDigest,
    pub token_count: u32,
    pub conservative_normalization_revision: u16,
    pub conservative_digest: ManifestDigest,
    pub conservative_tokens: Vec<ConservativeCloneTokenV1>,
    pub tokenization_status: CloneBodyTokenizationStatusV1,
    pub tokenization_issues: Vec<CloneBodyTokenizationIssueV1>,
    pub rename_normalization_revision: Option<u16>,
    pub rename_digest: Option<ManifestDigest>,
    pub rename_tokens: Option<Vec<ConservativeCloneTokenV1>>,
    pub rename_coverage: CloneBodyRenameStatusV1,
    pub rename_issues: Vec<CloneBodyRenameIssueV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CloneBodyOccurrenceV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: Option<WorktreeId>,
    pub source_generation: CodeGenerationId,
    pub snapshot_digest: ManifestDigest,
    pub symbol_occurrence_id: SymbolOccurrenceId,
    pub path: String,
    pub body_span: SourceSpan,
    pub payload_digest: ManifestDigest,
    pub eligibility: CloneBodyEligibilityV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexCloneBodyV1 {
    pub payload: CloneBodyPayloadV1,
    pub occurrence: CloneBodyOccurrenceV1,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClonePayloadBuildStatsV1 {
    pub(crate) reused: u64,
    pub(crate) computed: u64,
}

#[derive(Hash, PartialEq, Eq)]
struct ClonePayloadReuseKeyV1<'a> {
    language: &'a str,
    symbol_kind: &'a str,
    token_count: u32,
    conservative_revision: u16,
    conservative_tokens: &'a [ConservativeCloneTokenV1],
    tokenization_status: CloneBodyTokenizationStatusV1,
    tokenization_issues: &'a [CloneBodyTokenizationIssueV1],
    rename_revision: Option<u16>,
    rename_tokens: Option<&'a [ConservativeCloneTokenV1]>,
    rename_coverage: CloneBodyRenameStatusV1,
    rename_issues: &'a [CloneBodyRenameIssueV1],
}

struct ClonePayloadReuseIndexV1<'a>(HashMap<ClonePayloadReuseKeyV1<'a>, &'a CloneBodyPayloadV1>);

pub(crate) struct ClonePayloadBuildContextV1<'a> {
    prior: ClonePayloadReuseIndexV1<'a>,
    stats: ClonePayloadBuildStatsV1,
}

impl<'a> ClonePayloadBuildContextV1<'a> {
    pub(crate) fn new(prior: Option<&'a [CodeIndexCloneBodyV1]>) -> Self {
        Self {
            prior: ClonePayloadReuseIndexV1::new(prior),
            stats: ClonePayloadBuildStatsV1::default(),
        }
    }

    pub(crate) fn payload(
        &mut self,
        extracted: &ExtractedCloneBodyV1,
    ) -> Result<CloneBodyPayloadV1, String> {
        match self.prior.payload_for(extracted) {
            Some(payload) => {
                self.stats.reused = self.stats.reused.saturating_add(1);
                Ok(payload.clone())
            }
            None => {
                self.stats.computed = self.stats.computed.saturating_add(1);
                CloneBodyPayloadV1::from_extracted(extracted)
            }
        }
    }

    pub(crate) fn stats(&self) -> ClonePayloadBuildStatsV1 {
        self.stats
    }
}

impl<'a> ClonePayloadReuseIndexV1<'a> {
    fn new(prior: Option<&'a [CodeIndexCloneBodyV1]>) -> Self {
        Self(
            prior
                .unwrap_or_default()
                .iter()
                .map(|body| {
                    (
                        ClonePayloadReuseKeyV1::from_payload(&body.payload),
                        &body.payload,
                    )
                })
                .collect(),
        )
    }

    fn payload_for(&self, extracted: &ExtractedCloneBodyV1) -> Option<&'a CloneBodyPayloadV1> {
        self.0
            .get(&ClonePayloadReuseKeyV1::from_extracted(extracted))
            .copied()
    }
}

impl<'a> ClonePayloadReuseKeyV1<'a> {
    fn from_extracted(body: &'a ExtractedCloneBodyV1) -> Self {
        Self {
            language: &body.language,
            symbol_kind: body.symbol_kind.as_str(),
            token_count: body.non_trivia_token_count,
            conservative_revision: body.normalization_revision,
            conservative_tokens: &body.conservative_tokens,
            tokenization_status: body.tokenization_status,
            tokenization_issues: &body.tokenization_issues,
            rename_revision: body.rename_normalization_revision,
            rename_tokens: body.rename_tokens.as_deref(),
            rename_coverage: body.rename_status,
            rename_issues: &body.rename_issues,
        }
    }

    fn from_payload(payload: &'a CloneBodyPayloadV1) -> Self {
        Self {
            language: &payload.language,
            symbol_kind: &payload.symbol_kind,
            token_count: payload.token_count,
            conservative_revision: payload.conservative_normalization_revision,
            conservative_tokens: &payload.conservative_tokens,
            tokenization_status: payload.tokenization_status,
            tokenization_issues: &payload.tokenization_issues,
            rename_revision: payload.rename_normalization_revision,
            rename_tokens: payload.rename_tokens.as_deref(),
            rename_coverage: payload.rename_coverage,
            rename_issues: &payload.rename_issues,
        }
    }
}

struct ClonePayloadDigestInputV1<'a> {
    language: &'a str,
    symbol_kind: &'a str,
    token_count: u32,
    conservative_revision: u16,
    conservative_tokens: &'a [ConservativeCloneTokenV1],
    tokenization_status: CloneBodyTokenizationStatusV1,
    tokenization_issues: &'a [CloneBodyTokenizationIssueV1],
    rename_revision: Option<u16>,
    rename_tokens: Option<&'a [ConservativeCloneTokenV1]>,
    rename_coverage: CloneBodyRenameStatusV1,
    rename_issues: &'a [CloneBodyRenameIssueV1],
}

struct ClonePayloadDigestsV1 {
    body: ManifestDigest,
    conservative: ManifestDigest,
    rename: Option<ManifestDigest>,
    payload: ManifestDigest,
}

fn clone_payload_digests(
    input: ClonePayloadDigestInputV1<'_>,
) -> Result<ClonePayloadDigestsV1, String> {
    let body = canonical_sha256(&(
        BODY_DIGEST_DOMAIN,
        input.language,
        input.symbol_kind,
        input.conservative_revision,
        input.conservative_tokens,
    ))
    .map_err(|error| error.to_string())?;
    let conservative = canonical_sha256(&(
        CONSERVATIVE_DIGEST_DOMAIN,
        input.language,
        input.symbol_kind,
        input.conservative_revision,
        input.conservative_tokens,
    ))
    .map_err(|error| error.to_string())?;
    let rename = if input.rename_coverage == CloneBodyRenameStatusV1::Complete
        && input.tokenization_status == CloneBodyTokenizationStatusV1::Complete
    {
        match (input.rename_revision, input.rename_tokens) {
            (Some(revision), Some(tokens)) => Some(
                canonical_sha256(&(
                    RENAME_DIGEST_DOMAIN,
                    input.language,
                    input.symbol_kind,
                    revision,
                    tokens,
                ))
                .map_err(|error| error.to_string())?,
            ),
            _ => return Err("complete rename payload is missing tokens or revision".to_owned()),
        }
    } else {
        None
    };
    let payload = canonical_sha256(&(
        PAYLOAD_DIGEST_DOMAIN,
        input.language,
        input.symbol_kind,
        &body,
        input.token_count,
        input.conservative_revision,
        &conservative,
        input.conservative_tokens,
        input.tokenization_status,
        input.tokenization_issues,
        input.rename_revision,
        &rename,
        input.rename_tokens,
        input.rename_coverage,
        input.rename_issues,
    ))
    .map_err(|error| error.to_string())?;
    Ok(ClonePayloadDigestsV1 {
        body,
        conservative,
        rename,
        payload,
    })
}

impl CodeIndexCloneBodyV1 {
    pub fn retained_owned_bytes(&self) -> usize {
        fn token_bytes(tokens: &[ConservativeCloneTokenV1]) -> usize {
            tokens
                .iter()
                .fold(std::mem::size_of_val(tokens), |bytes, token| match token {
                    ConservativeCloneTokenV1::StructureStart { syntax_kind }
                    | ConservativeCloneTokenV1::StructureEnd { syntax_kind } => {
                        bytes.saturating_add(syntax_kind.capacity())
                    }
                    ConservativeCloneTokenV1::Syntax { syntax_kind, text } => bytes
                        .saturating_add(syntax_kind.capacity())
                        .saturating_add(text.capacity()),
                })
        }
        token_bytes(&self.payload.conservative_tokens)
            .saturating_add(self.payload.rename_tokens.as_deref().map_or(0, token_bytes))
            .saturating_add(self.payload.language.capacity())
            .saturating_add(self.payload.symbol_kind.capacity())
            .saturating_add(
                self.payload
                    .tokenization_issues
                    .capacity()
                    .saturating_mul(std::mem::size_of::<CloneBodyTokenizationIssueV1>()),
            )
            .saturating_add(
                self.payload
                    .rename_issues
                    .capacity()
                    .saturating_mul(std::mem::size_of::<CloneBodyRenameIssueV1>()),
            )
            .saturating_add(self.payload.payload_digest.as_str().len())
            .saturating_add(self.payload.body_digest.as_str().len())
            .saturating_add(self.payload.conservative_digest.as_str().len())
            .saturating_add(
                self.payload
                    .rename_digest
                    .as_ref()
                    .map_or(0, |digest| digest.as_str().len()),
            )
            .saturating_add(self.occurrence.project_id.as_str().len())
            .saturating_add(self.occurrence.repository_id.as_str().len())
            .saturating_add(
                self.occurrence
                    .worktree_id
                    .as_ref()
                    .map_or(0, |worktree| worktree.as_str().len()),
            )
            .saturating_add(self.occurrence.source_generation.as_str().len())
            .saturating_add(self.occurrence.snapshot_digest.as_str().len())
            .saturating_add(self.occurrence.symbol_occurrence_id.as_str().len())
            .saturating_add(self.occurrence.path.capacity())
            .saturating_add(self.occurrence.payload_digest.as_str().len())
    }
}

impl CloneBodyPayloadV1 {
    pub fn from_extracted(body: &ExtractedCloneBodyV1) -> Result<Self, String> {
        let digests = clone_payload_digests(ClonePayloadDigestInputV1 {
            language: body.language.as_str(),
            symbol_kind: body.symbol_kind.as_str(),
            token_count: body.non_trivia_token_count,
            conservative_revision: body.normalization_revision,
            conservative_tokens: &body.conservative_tokens,
            tokenization_status: body.tokenization_status,
            tokenization_issues: &body.tokenization_issues,
            rename_revision: body.rename_normalization_revision,
            rename_tokens: body.rename_tokens.as_deref(),
            rename_coverage: body.rename_status,
            rename_issues: &body.rename_issues,
        })?;
        Ok(Self {
            payload_digest: digests.payload,
            language: body.language.clone(),
            symbol_kind: body.symbol_kind.as_str().to_owned(),
            body_digest: digests.body,
            token_count: body.non_trivia_token_count,
            conservative_normalization_revision: body.normalization_revision,
            conservative_digest: digests.conservative,
            conservative_tokens: body.conservative_tokens.clone(),
            tokenization_status: body.tokenization_status,
            tokenization_issues: body.tokenization_issues.clone(),
            rename_normalization_revision: body.rename_normalization_revision,
            rename_digest: digests.rename,
            rename_tokens: body.rename_tokens.clone(),
            rename_coverage: body.rename_status,
            rename_issues: body.rename_issues.clone(),
        })
    }

    pub fn exact_keys(&self, eligibility: CloneBodyEligibilityV1) -> Vec<CloneExactKeyV1> {
        if eligibility != CloneBodyEligibilityV1::Eligible
            || self.tokenization_status != CloneBodyTokenizationStatusV1::Complete
        {
            return Vec::new();
        }
        let mut keys = vec![CloneExactKeyV1 {
            class: CloneNormalizationClassV1::Conservative,
            normalization_revision: self.conservative_normalization_revision,
            digest: self.conservative_digest.clone(),
        }];
        if let (Some(normalization_revision), Some(digest)) = (
            self.rename_normalization_revision,
            self.rename_digest.clone(),
        ) {
            keys.push(CloneExactKeyV1 {
                class: CloneNormalizationClassV1::Rename,
                normalization_revision,
                digest,
            });
        }
        keys
    }

    pub fn fingerprint_stream(
        &self,
        eligibility: CloneBodyEligibilityV1,
    ) -> Option<CloneFingerprintStreamV1<'_>> {
        if eligibility != CloneBodyEligibilityV1::Eligible {
            return None;
        }
        self.explicit_comparison_stream()
    }

    fn explicit_comparison_stream(&self) -> Option<CloneFingerprintStreamV1<'_>> {
        if self.tokenization_status != CloneBodyTokenizationStatusV1::Complete {
            return None;
        }
        if self.rename_coverage == CloneBodyRenameStatusV1::Complete
            && let (Some(normalization_revision), Some(tokens)) = (
                self.rename_normalization_revision,
                self.rename_tokens.as_deref(),
            )
        {
            return Some(CloneFingerprintStreamV1 {
                class: CloneNormalizationClassV1::Rename,
                normalization_revision,
                tokens,
                rename_tier_unavailable: None,
            });
        }
        Some(CloneFingerprintStreamV1 {
            class: CloneNormalizationClassV1::Conservative,
            normalization_revision: self.conservative_normalization_revision,
            tokens: &self.conservative_tokens,
            rename_tier_unavailable: Some(self.rename_coverage),
        })
    }

    pub fn fingerprint_positions(
        &self,
        eligibility: CloneBodyEligibilityV1,
    ) -> Result<Vec<CloneFingerprintPositionV1>, String> {
        let Some(stream) = self.fingerprint_stream(eligibility) else {
            return Ok(Vec::new());
        };
        winnow_clone_tokens(stream.tokens)
    }

    pub fn validate(&self) -> Result<(), String> {
        let digests = clone_payload_digests(ClonePayloadDigestInputV1 {
            language: &self.language,
            symbol_kind: &self.symbol_kind,
            token_count: self.token_count,
            conservative_revision: self.conservative_normalization_revision,
            conservative_tokens: &self.conservative_tokens,
            tokenization_status: self.tokenization_status,
            tokenization_issues: &self.tokenization_issues,
            rename_revision: self.rename_normalization_revision,
            rename_tokens: self.rename_tokens.as_deref(),
            rename_coverage: self.rename_coverage,
            rename_issues: &self.rename_issues,
        })?;
        if self.body_digest != digests.body
            || self.conservative_digest != digests.conservative
            || self.rename_digest != digests.rename
            || self.payload_digest != digests.payload
        {
            return Err("clone payload digests do not match their canonical tokens".to_owned());
        }
        Ok(())
    }
}

impl CloneSelectedBlockV1 {
    pub fn from_payload(
        source: &CloneBodyPayloadV1,
        source_eligibility: CloneBodyEligibilityV1,
        token_range: Range<usize>,
    ) -> Result<Self, String> {
        source.validate()?;
        if source_eligibility == CloneBodyEligibilityV1::ExcludedIncompleteTokenization {
            return Err("selected clone block source has incomplete tokenization".to_owned());
        }
        let stream = source
            .explicit_comparison_stream()
            .ok_or_else(|| "selected clone block source has no complete token stream".to_owned())?;
        let tokens = stream
            .tokens
            .get(token_range)
            .filter(|tokens| !tokens.is_empty())
            .ok_or_else(|| {
                "selected clone block token range is empty or outside its source".to_owned()
            })?
            .to_vec();
        if winnow_clone_tokens(&tokens)?.is_empty() {
            return Err("selected clone block is too small for fingerprint lookup".to_owned());
        }
        Ok(Self {
            source_payload_digest: source.payload_digest.clone(),
            source_body_digest: source.body_digest.clone(),
            language: source.language.clone(),
            class: stream.class,
            normalization_revision: stream.normalization_revision,
            rename_tier_unavailable: stream.rename_tier_unavailable,
            tokens,
        })
    }

    pub fn source_payload_digest(&self) -> &ManifestDigest {
        &self.source_payload_digest
    }

    pub fn source_body_digest(&self) -> &ManifestDigest {
        &self.source_body_digest
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    pub fn class(&self) -> CloneNormalizationClassV1 {
        self.class
    }

    pub fn normalization_revision(&self) -> u16 {
        self.normalization_revision
    }

    pub fn rename_tier_unavailable(&self) -> Option<CloneBodyRenameStatusV1> {
        self.rename_tier_unavailable
    }

    pub fn tokens(&self) -> &[ConservativeCloneTokenV1] {
        &self.tokens
    }

    pub fn fingerprint_positions(&self) -> Result<Vec<CloneFingerprintPositionV1>, String> {
        winnow_clone_tokens(&self.tokens)
    }
}

fn winnow_clone_tokens(
    tokens: &[ConservativeCloneTokenV1],
) -> Result<Vec<CloneFingerprintPositionV1>, String> {
    if tokens.len() < CLONE_FINGERPRINT_K_V1 + CLONE_FINGERPRINT_WINDOW_V1 - 1 {
        return Ok(Vec::new());
    }
    let hashes = tokens
        .windows(CLONE_FINGERPRINT_K_V1)
        .map(|window| {
            let bytes = canonical_json_bytes(&(FINGERPRINT_DOMAIN, window))
                .map_err(|error| error.to_string())?;
            let digest = Sha256::digest(bytes);
            let prefix: [u8; 8] = digest[..8]
                .try_into()
                .map_err(|error: std::array::TryFromSliceError| error.to_string())?;
            Ok(u64::from_be_bytes(prefix) & i64::MAX as u64)
        })
        .collect::<Result<Vec<_>, String>>()?;
    select_rightmost_minima(&hashes)
        .into_iter()
        .map(|token_position| {
            Ok(CloneFingerprintPositionV1 {
                fingerprint: hashes[token_position],
                token_position: u32::try_from(token_position).map_err(|error| error.to_string())?,
            })
        })
        .collect()
}

fn select_rightmost_minima(hashes: &[u64]) -> Vec<usize> {
    if hashes.len() < CLONE_FINGERPRINT_WINDOW_V1 {
        return Vec::new();
    }
    let mut selected = Vec::new();
    for (window_start, window) in hashes.windows(CLONE_FINGERPRINT_WINDOW_V1).enumerate() {
        let mut minimum = 0usize;
        for position in 1..window.len() {
            if window[position] <= window[minimum] {
                minimum = position;
            }
        }
        let position = window_start + minimum;
        if selected.last() != Some(&position) {
            selected.push(position);
        }
    }
    selected
}

pub fn verify_clone_token_anchor(
    left: &[ConservativeCloneTokenV1],
    left_position: u32,
    right: &[ConservativeCloneTokenV1],
    right_position: u32,
) -> bool {
    let Ok(left_position) = usize::try_from(left_position) else {
        return false;
    };
    let Ok(right_position) = usize::try_from(right_position) else {
        return false;
    };
    let Some(left) = left.get(left_position..left_position.saturating_add(CLONE_FINGERPRINT_K_V1))
    else {
        return false;
    };
    let Some(right) =
        right.get(right_position..right_position.saturating_add(CLONE_FINGERPRINT_K_V1))
    else {
        return false;
    };
    left == right
}

pub fn align_clone_tokens(
    left: &[ConservativeCloneTokenV1],
    right: &[ConservativeCloneTokenV1],
    anchors: &[CloneTokenAnchorV1],
    maximum_work: u64,
    should_stop: impl FnMut() -> bool,
) -> Result<CloneWholeBodyAlignmentV1, CloneAlignmentStoppedV1> {
    let mut meter = CloneAlignmentWorkMeterV1 {
        maximum: maximum_work,
        work: 0,
        should_stop,
    };
    let ordered_anchors = chain_clone_anchors(left, right, anchors, &mut meter)?;
    let mut differences = Vec::new();
    let mut shared_token_count = 0usize;
    let mut left_start = 0usize;
    let mut right_start = 0usize;
    for anchor in &ordered_anchors {
        let left_anchor =
            usize::try_from(anchor.left_token_position).map_err(|_| meter.exhausted())?;
        let right_anchor =
            usize::try_from(anchor.right_token_position).map_err(|_| meter.exhausted())?;
        let (mut segment_differences, segment_shared) = diff_clone_token_segment(
            &left[left_start..left_anchor],
            &right[right_start..right_anchor],
            left_start,
            right_start,
            &mut meter,
        )?;
        differences.append(&mut segment_differences);
        shared_token_count = shared_token_count
            .saturating_add(segment_shared)
            .saturating_add(CLONE_FINGERPRINT_K_V1);
        left_start = left_anchor.saturating_add(CLONE_FINGERPRINT_K_V1);
        right_start = right_anchor.saturating_add(CLONE_FINGERPRINT_K_V1);
    }
    let (mut tail_differences, tail_shared) = diff_clone_token_segment(
        &left[left_start..],
        &right[right_start..],
        left_start,
        right_start,
        &mut meter,
    )?;
    differences.append(&mut tail_differences);
    shared_token_count = shared_token_count.saturating_add(tail_shared);
    let shared_token_count = u32::try_from(shared_token_count).map_err(|_| meter.exhausted())?;
    let left_coverage_millionths =
        directional_coverage(shared_token_count, left.len()).ok_or_else(|| meter.exhausted())?;
    let right_coverage_millionths =
        directional_coverage(shared_token_count, right.len()).ok_or_else(|| meter.exhausted())?;
    Ok(CloneWholeBodyAlignmentV1 {
        ordered_anchors,
        shared_token_count,
        left_coverage_millionths,
        right_coverage_millionths,
        differences,
        work: meter.work,
    })
}

struct CloneAlignmentWorkMeterV1<F> {
    maximum: u64,
    work: u64,
    should_stop: F,
}

impl<F: FnMut() -> bool> CloneAlignmentWorkMeterV1<F> {
    fn tick(&mut self) -> Result<(), CloneAlignmentStoppedV1> {
        if self.work == self.maximum {
            return Err(self.exhausted());
        }
        self.work = self.work.saturating_add(1);
        if (self.work == 1 || self.work.is_multiple_of(1_024)) && (self.should_stop)() {
            return Err(CloneAlignmentStoppedV1 {
                reason: CloneAlignmentStopReasonV1::Interrupted,
                work: self.work,
            });
        }
        Ok(())
    }

    fn exhausted(&self) -> CloneAlignmentStoppedV1 {
        CloneAlignmentStoppedV1 {
            reason: CloneAlignmentStopReasonV1::WorkBudgetExhausted,
            work: self.work,
        }
    }

    fn remaining(&self) -> u64 {
        self.maximum.saturating_sub(self.work)
    }
}

fn chain_clone_anchors<F: FnMut() -> bool>(
    left: &[ConservativeCloneTokenV1],
    right: &[ConservativeCloneTokenV1],
    anchors: &[CloneTokenAnchorV1],
    meter: &mut CloneAlignmentWorkMeterV1<F>,
) -> Result<Vec<CloneTokenAnchorV1>, CloneAlignmentStoppedV1> {
    let mut verified_anchors = Vec::with_capacity(anchors.len());
    for anchor in anchors {
        if verify_clone_token_anchor_with_meter(left, right, anchor, meter)? {
            verified_anchors.push(*anchor);
        }
    }
    let mut anchors = verified_anchors;
    anchors.sort_unstable_by_key(|anchor| {
        (
            anchor.left_token_position,
            anchor.right_token_position,
            anchor.fingerprint,
        )
    });
    anchors.dedup();
    if anchors.is_empty() {
        return Ok(Vec::new());
    }
    let mut lengths = vec![1usize; anchors.len()];
    let mut previous = vec![None; anchors.len()];
    for current in 0..anchors.len() {
        meter.tick()?;
        for candidate in 0..current {
            meter.tick()?;
            if anchors[candidate]
                .left_token_position
                .saturating_add(CLONE_FINGERPRINT_K_V1 as u32)
                <= anchors[current].left_token_position
                && anchors[candidate]
                    .right_token_position
                    .saturating_add(CLONE_FINGERPRINT_K_V1 as u32)
                    <= anchors[current].right_token_position
                && lengths[candidate].saturating_add(1) > lengths[current]
            {
                lengths[current] = lengths[candidate].saturating_add(1);
                previous[current] = Some(candidate);
            }
        }
    }
    let mut cursor = lengths
        .iter()
        .enumerate()
        .max_by_key(|(index, length)| (**length, std::cmp::Reverse(*index)))
        .map(|(index, _)| index)
        .ok_or_else(|| meter.exhausted())?;
    let mut chain = Vec::with_capacity(lengths[cursor]);
    loop {
        chain.push(anchors[cursor]);
        let Some(parent) = previous[cursor] else {
            break;
        };
        cursor = parent;
    }
    chain.reverse();
    Ok(chain)
}

fn verify_clone_token_anchor_with_meter<F: FnMut() -> bool>(
    left: &[ConservativeCloneTokenV1],
    right: &[ConservativeCloneTokenV1],
    anchor: &CloneTokenAnchorV1,
    meter: &mut CloneAlignmentWorkMeterV1<F>,
) -> Result<bool, CloneAlignmentStoppedV1> {
    meter.tick()?;
    let Some(left) = usize::try_from(anchor.left_token_position)
        .ok()
        .and_then(|position| left.get(position..position.saturating_add(CLONE_FINGERPRINT_K_V1)))
    else {
        return Ok(false);
    };
    let Some(right) = usize::try_from(anchor.right_token_position)
        .ok()
        .and_then(|position| right.get(position..position.saturating_add(CLONE_FINGERPRINT_K_V1)))
    else {
        return Ok(false);
    };
    for (left, right) in left.iter().zip(right) {
        meter.tick()?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

fn diff_clone_token_segment<F: FnMut() -> bool>(
    left: &[ConservativeCloneTokenV1],
    right: &[ConservativeCloneTokenV1],
    left_offset: usize,
    right_offset: usize,
    meter: &mut CloneAlignmentWorkMeterV1<F>,
) -> Result<(Vec<CloneAlignedDifferenceV1>, usize), CloneAlignmentStoppedV1> {
    let mut prefix = 0usize;
    while prefix < left.len() && prefix < right.len() {
        meter.tick()?;
        if left[prefix] != right[prefix] {
            break;
        }
        prefix = prefix.saturating_add(1);
    }
    let mut suffix = 0usize;
    while suffix < left.len().saturating_sub(prefix) && suffix < right.len().saturating_sub(prefix)
    {
        meter.tick()?;
        if left[left.len() - suffix - 1] != right[right.len() - suffix - 1] {
            break;
        }
        suffix = suffix.saturating_add(1);
    }
    let left_middle = &left[prefix..left.len() - suffix];
    let right_middle = &right[prefix..right.len() - suffix];
    if left_middle.is_empty() && right_middle.is_empty() {
        return Ok((Vec::new(), left.len()));
    }
    if left_middle.is_empty() || right_middle.is_empty() {
        for _ in 0..left_middle.len().saturating_add(right_middle.len()) {
            meter.tick()?;
        }
        return Ok((
            vec![clone_difference(
                left_middle,
                right_middle,
                left_offset.saturating_add(prefix),
                right_offset.saturating_add(prefix),
            )?],
            prefix.saturating_add(suffix),
        ));
    }
    let maximum = left_middle.len().saturating_add(right_middle.len());
    if u64::try_from(maximum).map_or(true, |maximum| maximum > meter.remaining()) {
        return Err(meter.exhausted());
    }
    let (removed, added) = myers_clone_diff(left_middle, right_middle, meter)?;
    let removed_count = removed.iter().filter(|removed| **removed).count();
    let mut differences = Vec::new();
    let mut left_position = 0usize;
    let mut right_position = 0usize;
    while left_position < left_middle.len() || right_position < right_middle.len() {
        if left_position < left_middle.len()
            && right_position < right_middle.len()
            && !removed[left_position]
            && !added[right_position]
        {
            left_position = left_position.saturating_add(1);
            right_position = right_position.saturating_add(1);
            continue;
        }
        let left_start = left_position;
        let right_start = right_position;
        while left_position < left_middle.len() && removed[left_position] {
            left_position = left_position.saturating_add(1);
        }
        while right_position < right_middle.len() && added[right_position] {
            right_position = right_position.saturating_add(1);
        }
        differences.push(clone_difference(
            &left_middle[left_start..left_position],
            &right_middle[right_start..right_position],
            left_offset
                .saturating_add(prefix)
                .saturating_add(left_start),
            right_offset
                .saturating_add(prefix)
                .saturating_add(right_start),
        )?);
    }
    Ok((
        differences,
        prefix
            .saturating_add(suffix)
            .saturating_add(left_middle.len().saturating_sub(removed_count)),
    ))
}

fn myers_clone_diff<F: FnMut() -> bool>(
    left: &[ConservativeCloneTokenV1],
    right: &[ConservativeCloneTokenV1],
    meter: &mut CloneAlignmentWorkMeterV1<F>,
) -> Result<(Vec<bool>, Vec<bool>), CloneAlignmentStoppedV1> {
    let maximum = left.len().saturating_add(right.len());
    let offset = isize::try_from(maximum.saturating_add(1)).map_err(|_| meter.exhausted())?;
    let frontier_len = maximum
        .checked_mul(2)
        .and_then(|length| length.checked_add(3))
        .ok_or_else(|| meter.exhausted())?;
    let mut frontier = vec![-1isize; frontier_len];
    frontier[usize::try_from(offset.saturating_add(1)).map_err(|_| meter.exhausted())?] = 0;
    let left_limit = isize::try_from(left.len()).map_err(|_| meter.exhausted())?;
    let right_limit = isize::try_from(right.len()).map_err(|_| meter.exhausted())?;
    let mut trace = Vec::new();
    for distance in 0..=maximum {
        let distance_isize = isize::try_from(distance).map_err(|_| meter.exhausted())?;
        let mut diagonal = -distance_isize;
        while diagonal <= distance_isize {
            meter.tick()?;
            let index =
                usize::try_from(offset.saturating_add(diagonal)).map_err(|_| meter.exhausted())?;
            let mut left_position = if diagonal == -distance_isize
                || (diagonal != distance_isize && frontier[index - 1] < frontier[index + 1])
            {
                frontier[index + 1]
            } else {
                frontier[index - 1].saturating_add(1)
            };
            let mut right_position = left_position.saturating_sub(diagonal);
            while left_position < left_limit
                && right_position < right_limit
                && left[usize::try_from(left_position).map_err(|_| meter.exhausted())?]
                    == right[usize::try_from(right_position).map_err(|_| meter.exhausted())?]
            {
                meter.tick()?;
                left_position = left_position.saturating_add(1);
                right_position = right_position.saturating_add(1);
            }
            frontier[index] = left_position;
            if left_position >= left_limit && right_position >= right_limit {
                trace.push(compact_clone_frontier(
                    &frontier,
                    offset,
                    distance_isize,
                    meter,
                )?);
                return backtrack_myers_clone_diff(
                    &trace,
                    distance,
                    left.len(),
                    right.len(),
                    meter,
                );
            }
            diagonal = diagonal.saturating_add(2);
        }
        trace.push(compact_clone_frontier(
            &frontier,
            offset,
            distance_isize,
            meter,
        )?);
    }
    Err(meter.exhausted())
}

fn compact_clone_frontier<F: FnMut() -> bool>(
    frontier: &[isize],
    offset: isize,
    distance: isize,
    meter: &CloneAlignmentWorkMeterV1<F>,
) -> Result<Vec<isize>, CloneAlignmentStoppedV1> {
    let start = usize::try_from(offset.saturating_sub(distance)).map_err(|_| meter.exhausted())?;
    let end = usize::try_from(offset.saturating_add(distance).saturating_add(1))
        .map_err(|_| meter.exhausted())?;
    frontier
        .get(start..end)
        .map(<[isize]>::to_vec)
        .ok_or_else(|| meter.exhausted())
}

fn backtrack_myers_clone_diff<F: FnMut() -> bool>(
    trace: &[Vec<isize>],
    distance: usize,
    left_len: usize,
    right_len: usize,
    meter: &CloneAlignmentWorkMeterV1<F>,
) -> Result<(Vec<bool>, Vec<bool>), CloneAlignmentStoppedV1> {
    let mut removed = vec![false; left_len];
    let mut added = vec![false; right_len];
    let mut left_position = isize::try_from(left_len).map_err(|_| meter.exhausted())?;
    let mut right_position = isize::try_from(right_len).map_err(|_| meter.exhausted())?;
    for current_distance in (1..=distance).rev() {
        let frontier = &trace[current_distance - 1];
        let current_distance_isize =
            isize::try_from(current_distance).map_err(|_| meter.exhausted())?;
        let previous_distance = current_distance_isize.saturating_sub(1);
        let diagonal = left_position.saturating_sub(right_position);
        let previous_diagonal = if diagonal == -current_distance_isize
            || (diagonal != current_distance_isize
                && compact_frontier_value(frontier, previous_distance, diagonal - 1, meter)?
                    < compact_frontier_value(frontier, previous_distance, diagonal + 1, meter)?)
        {
            diagonal.saturating_add(1)
        } else {
            diagonal.saturating_sub(1)
        };
        let previous_left =
            compact_frontier_value(frontier, previous_distance, previous_diagonal, meter)?;
        let previous_right = previous_left.saturating_sub(previous_diagonal);
        while left_position > previous_left && right_position > previous_right {
            left_position = left_position.saturating_sub(1);
            right_position = right_position.saturating_sub(1);
        }
        if left_position == previous_left {
            right_position = right_position.saturating_sub(1);
            added[usize::try_from(right_position).map_err(|_| meter.exhausted())?] = true;
        } else {
            left_position = left_position.saturating_sub(1);
            removed[usize::try_from(left_position).map_err(|_| meter.exhausted())?] = true;
        }
    }
    Ok((removed, added))
}

fn compact_frontier_value<F: FnMut() -> bool>(
    frontier: &[isize],
    distance: isize,
    diagonal: isize,
    meter: &CloneAlignmentWorkMeterV1<F>,
) -> Result<isize, CloneAlignmentStoppedV1> {
    let index =
        usize::try_from(diagonal.saturating_add(distance)).map_err(|_| meter.exhausted())?;
    frontier
        .get(index)
        .copied()
        .ok_or_else(|| meter.exhausted())
}

fn clone_difference(
    left: &[ConservativeCloneTokenV1],
    right: &[ConservativeCloneTokenV1],
    left_start: usize,
    right_start: usize,
) -> Result<CloneAlignedDifferenceV1, CloneAlignmentStoppedV1> {
    let span = |start: usize, length: usize| {
        Ok(CloneTokenSpanV1 {
            start: u32::try_from(start).map_err(|_| CloneAlignmentStoppedV1 {
                reason: CloneAlignmentStopReasonV1::WorkBudgetExhausted,
                work: 0,
            })?,
            end: u32::try_from(start.saturating_add(length)).map_err(|_| {
                CloneAlignmentStoppedV1 {
                    reason: CloneAlignmentStopReasonV1::WorkBudgetExhausted,
                    work: 0,
                }
            })?,
        })
    };
    Ok(CloneAlignedDifferenceV1 {
        left_span: span(left_start, left.len())?,
        right_span: span(right_start, right.len())?,
        left_tokens: left.to_vec(),
        right_tokens: right.to_vec(),
    })
}

fn directional_coverage(shared: u32, total: usize) -> Option<u32> {
    let total = u64::try_from(total).ok()?;
    if total == 0 {
        return Some(0);
    }
    u32::try_from(u64::from(shared).saturating_mul(1_000_000) / total).ok()
}

#[cfg(test)]
mod fingerprint_tests {
    use tracedecay_code_extraction::{
        CloneBodyRenameStatusV1, ConservativeCloneTokenV1, LanguageExtractor, TypeScriptExtractor,
    };

    use super::{
        CLONE_FINGERPRINT_K_V1, CLONE_FINGERPRINT_WINDOW_V1, CloneAlignmentStopReasonV1,
        CloneBodyPayloadV1, CloneNormalizationClassV1, CloneTokenAnchorV1, align_clone_tokens,
        select_rightmost_minima, verify_clone_token_anchor, winnow_clone_tokens,
    };

    fn tokens(prefix: &str, count: usize) -> Vec<ConservativeCloneTokenV1> {
        (0..count)
            .map(|ordinal| ConservativeCloneTokenV1::Syntax {
                syntax_kind: "identifier".to_owned(),
                text: format!("{prefix}{ordinal}"),
            })
            .collect()
    }

    #[test]
    fn standard_winnowing_observes_the_6_7_13_14_token_boundaries() {
        assert!(
            winnow_clone_tokens(&tokens("six", 6))
                .expect("winnow")
                .is_empty()
        );
        assert!(
            winnow_clone_tokens(&tokens("seven", 7))
                .expect("winnow")
                .is_empty()
        );
        assert!(
            winnow_clone_tokens(&tokens("thirteen", 13))
                .expect("winnow")
                .is_empty()
        );
        assert_eq!(
            winnow_clone_tokens(&tokens("fourteen", 14))
                .expect("winnow")
                .len(),
            1
        );
        assert_eq!(CLONE_FINGERPRINT_K_V1, 7);
        assert_eq!(CLONE_FINGERPRINT_WINDOW_V1, 8);
    }

    #[test]
    fn standard_winnowing_selects_the_rightmost_equal_minimum_once() {
        assert_eq!(select_rightmost_minima(&[9, 1, 1, 2, 3, 4, 5, 6]), vec![2]);
        assert_eq!(
            select_rightmost_minima(&[4, 4, 4, 4, 4, 4, 4, 4, 4, 4]),
            vec![7, 8, 9]
        );
        assert_eq!(
            select_rightmost_minima(&[9, 1, 2, 3, 4, 5, 6, 7, 8]),
            vec![1],
            "the same selected position must not be emitted by adjacent windows"
        );
    }

    #[test]
    fn a_shared_fourteen_token_run_has_a_common_fingerprint() {
        let shared = tokens("shared", 14);
        let mut left = tokens("left-prefix", 9);
        left.extend(shared.clone());
        left.extend(tokens("left-suffix", 9));
        let mut right = tokens("right-prefix", 9);
        right.extend(shared);
        right.extend(tokens("right-suffix", 9));

        let left = winnow_clone_tokens(&left).expect("left winnowing");
        let right = winnow_clone_tokens(&right).expect("right winnowing");
        assert!(
            left.iter().any(|left| right
                .iter()
                .any(|right| left.fingerprint == right.fingerprint)),
            "the k+w-1 shared-token guarantee must hold at the fingerprint layer"
        );
    }

    #[test]
    fn a_forced_fingerprint_collision_cannot_verify_different_token_bytes() {
        let left = tokens("left", CLONE_FINGERPRINT_K_V1);
        let mut right = left.clone();
        right[3] = ConservativeCloneTokenV1::Syntax {
            syntax_kind: "identifier".to_owned(),
            text: "different".to_owned(),
        };

        assert!(!verify_clone_token_anchor(&left, 0, &right, 0));
        assert!(verify_clone_token_anchor(&left, 0, &left, 0));
    }

    #[test]
    fn rename_stream_requires_complete_binding_normalization() {
        let complete = TypeScriptExtractor.extract_artifact(
            "src/complete.ts",
            "function copy(input) { const one = parse(input); const two = use(one); const three = use(two); return finish(three, input, one, two); }",
        );
        let complete = complete.clone_bodies.first().expect("complete body");
        let complete_payload = CloneBodyPayloadV1::from_extracted(complete).expect("payload");
        let complete_stream = complete_payload
            .fingerprint_stream(complete.eligibility)
            .expect("complete stream");
        assert_eq!(complete_stream.class, CloneNormalizationClassV1::Rename);
        assert_eq!(complete_stream.rename_tier_unavailable, None);

        let partial = TypeScriptExtractor.extract_artifact(
            "src/partial.js",
            "function copy(input) { const one = eval(input); const two = use(one); const three = use(two); return finish(three, input, one, two); }",
        );
        let partial = partial.clone_bodies.first().expect("partial body");
        assert_eq!(partial.rename_status, CloneBodyRenameStatusV1::Partial);
        let partial_payload = CloneBodyPayloadV1::from_extracted(partial).expect("payload");
        let partial_stream = partial_payload
            .fingerprint_stream(partial.eligibility)
            .expect("conservative stream");
        assert_eq!(
            partial_stream.class,
            CloneNormalizationClassV1::Conservative
        );
        assert_eq!(
            partial_stream.rename_tier_unavailable,
            Some(CloneBodyRenameStatusV1::Partial)
        );
    }

    #[test]
    fn a_forced_body_digest_collision_cannot_validate_other_token_bytes() {
        let extracted = TypeScriptExtractor.extract_artifact(
            "src/body.ts",
            "function copy(input) { const one = parse(input); const two = use(one); const three = use(two); return finish(three, input, one, two); }",
        );
        let body = extracted.clone_bodies.first().expect("clone body");
        let mut payload = CloneBodyPayloadV1::from_extracted(body).expect("payload");
        payload.conservative_tokens[0] = ConservativeCloneTokenV1::Syntax {
            syntax_kind: "identifier".to_owned(),
            text: "colliding-but-different".to_owned(),
        };
        assert!(payload.validate().is_err());
    }

    #[test]
    fn anchored_alignment_reports_insertions_and_literal_changes_directionally() {
        let left = tokens("token", 30);
        let mut right = left[..10].to_vec();
        right.extend(tokens("added-branch", 2));
        right.extend(left[10..20].iter().cloned());
        right.push(ConservativeCloneTokenV1::Syntax {
            syntax_kind: "string".to_owned(),
            text: "\"changed\"".to_owned(),
        });
        right.extend(left[21..].iter().cloned());
        let anchors = [
            CloneTokenAnchorV1 {
                fingerprint: 1,
                left_token_position: 0,
                right_token_position: 0,
            },
            CloneTokenAnchorV1 {
                fingerprint: 2,
                left_token_position: 12,
                right_token_position: 14,
            },
            CloneTokenAnchorV1 {
                fingerprint: 3,
                left_token_position: 22,
                right_token_position: 24,
            },
        ];

        let alignment = align_clone_tokens(&left, &right, &anchors, 2_000_000, || false)
            .expect("bounded alignment");

        assert_eq!(alignment.shared_token_count, 29);
        assert_eq!(alignment.left_coverage_millionths, 966_666);
        assert_eq!(alignment.right_coverage_millionths, 906_250);
        assert_eq!(alignment.ordered_anchors, anchors);
        assert_eq!(alignment.differences.len(), 2);
        assert!(alignment.differences[0].left_tokens.is_empty());
        assert_eq!(
            alignment.differences[0].right_tokens,
            tokens("added-branch", 2)
        );
        assert_eq!(alignment.differences[1].left_tokens, vec![left[20].clone()]);
        assert_eq!(
            alignment.differences[1].right_tokens,
            vec![ConservativeCloneTokenV1::Syntax {
                syntax_kind: "string".to_owned(),
                text: "\"changed\"".to_owned(),
            }]
        );
        assert!(alignment.work > 0);
        assert!(alignment.work <= 2_000_000);
    }

    #[test]
    fn alignment_stops_at_work_and_cancellation_boundaries() {
        let left = tokens("left", 30);
        let right = tokens("right", 30);

        let exhausted = align_clone_tokens(&left, &right, &[], 1, || false)
            .expect_err("one step cannot align unrelated bodies");
        assert_eq!(
            exhausted.reason,
            CloneAlignmentStopReasonV1::WorkBudgetExhausted
        );
        assert_eq!(exhausted.work, 1);

        let interrupted = align_clone_tokens(&left, &right, &[], 2_000_000, || true)
            .expect_err("alignment observes caller cancellation");
        assert_eq!(interrupted.reason, CloneAlignmentStopReasonV1::Interrupted);
        assert_eq!(interrupted.work, 1);
    }

    #[test]
    fn anchor_validation_consumes_alignment_work_budget() {
        let body = tokens("shared", CLONE_FINGERPRINT_K_V1);
        let anchors = [CloneTokenAnchorV1 {
            fingerprint: 1,
            left_token_position: 0,
            right_token_position: 0,
        }];

        let stopped = align_clone_tokens(
            &body,
            &body,
            &anchors,
            (CLONE_FINGERPRINT_K_V1 - 1) as u64,
            || false,
        )
        .expect_err("anchor token comparisons must consume the work budget");

        assert_eq!(
            stopped.reason,
            CloneAlignmentStopReasonV1::WorkBudgetExhausted
        );
        assert_eq!(stopped.work, (CLONE_FINGERPRINT_K_V1 - 1) as u64);
    }
}
