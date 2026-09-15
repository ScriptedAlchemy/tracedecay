use std::collections::HashMap;

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
        if eligibility != CloneBodyEligibilityV1::Eligible
            || self.tokenization_status != CloneBodyTokenizationStatusV1::Complete
        {
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

#[cfg(test)]
mod fingerprint_tests {
    use tracedecay_code_extraction::{
        CloneBodyRenameStatusV1, ConservativeCloneTokenV1, LanguageExtractor, TypeScriptExtractor,
    };

    use super::{
        CLONE_FINGERPRINT_K_V1, CLONE_FINGERPRINT_WINDOW_V1, CloneBodyPayloadV1,
        CloneNormalizationClassV1, select_rightmost_minima, verify_clone_token_anchor,
        winnow_clone_tokens,
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
}
