use serde::{Deserialize, Serialize};
use tracedecay_code_extraction::{
    CloneBodyEligibilityV1, CloneBodyRenameIssueV1, CloneBodyRenameStatusV1,
    CloneBodyTokenizationIssueV1, CloneBodyTokenizationStatusV1, ConservativeCloneTokenV1,
    ExtractedCloneBodyV1,
};
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, SourceSpan, SymbolOccurrenceId,
    WorktreeId, canonical_sha256,
};

const BODY_DIGEST_DOMAIN: &str = "tracedecay.clone-body.v1";
const CONSERVATIVE_DIGEST_DOMAIN: &str = "tracedecay.clone-conservative.v1";
const RENAME_DIGEST_DOMAIN: &str = "tracedecay.clone-rename.v1";
const PAYLOAD_DIGEST_DOMAIN: &str = "tracedecay.clone-payload.v1";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum CloneNormalizationClassV1 {
    Conservative = 1,
    Rename = 2,
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
