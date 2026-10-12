use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_code_extraction::ExtractedCloneBodyV1;
pub use tracedecay_code_extraction::{
    CloneBodyEligibilityV1, CloneBodyRenameIssueV1, CloneBodyRenameStatusV1,
    CloneBodyTokenizationIssueV1, CloneBodyTokenizationStatusV1, CloneTokenStreamV1,
    ConservativeCloneTokenV1,
};
use tracedecay_contracts::retrieval::SimilarOccurrenceV1;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, SourceSpan, SymbolOccurrenceId,
    WorktreeId, canonical_json_bytes,
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
    pub conservative_tokens: CloneTokenStreamV1,
    pub tokenization_status: CloneBodyTokenizationStatusV1,
    pub tokenization_issues: Vec<CloneBodyTokenizationIssueV1>,
    pub rename_normalization_revision: Option<u16>,
    pub rename_digest: Option<ManifestDigest>,
    pub rename_tokens: Option<CloneTokenStreamV1>,
    pub rename_coverage: CloneBodyRenameStatusV1,
    pub rename_issues: Vec<CloneBodyRenameIssueV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CloneBodyOccurrenceV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    /// `None` inside a generation's file pages, which name repository content
    /// shared by every linked worktree sealing it; a serving route that
    /// rebuilds an occurrence supplies its own worktree.
    pub worktree_id: Option<WorktreeId>,
    pub source_generation: CodeGenerationId,
    pub snapshot_digest: ManifestDigest,
    pub symbol_occurrence_id: SymbolOccurrenceId,
    pub path: String,
    pub body_span: SourceSpan,
    pub payload_digest: ManifestDigest,
    pub eligibility: CloneBodyEligibilityV1,
}

impl CloneBodyOccurrenceV1 {
    /// The public similar-read projection of this occurrence.
    pub fn similar_occurrence(&self) -> SimilarOccurrenceV1 {
        SimilarOccurrenceV1 {
            project_id: self.project_id.clone(),
            repository_id: self.repository_id.clone(),
            worktree_id: self.worktree_id.clone(),
            source_generation: self.source_generation.clone(),
            snapshot_digest: self.snapshot_digest.clone(),
            symbol_occurrence_id: self.symbol_occurrence_id.clone(),
            path: self.path.clone(),
            body_span: self.body_span,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexCloneBodyV1 {
    pub payload: Arc<CloneBodyPayloadV1>,
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
    conservative_tokens: &'a CloneTokenStreamV1,
    tokenization_status: CloneBodyTokenizationStatusV1,
    tokenization_issues: &'a [CloneBodyTokenizationIssueV1],
    rename_revision: Option<u16>,
    rename_tokens: Option<&'a CloneTokenStreamV1>,
    rename_coverage: CloneBodyRenameStatusV1,
    rename_issues: &'a [CloneBodyRenameIssueV1],
}

struct ClonePayloadReuseIndexV1<'a>(
    HashMap<ClonePayloadReuseKeyV1<'a>, &'a Arc<CloneBodyPayloadV1>>,
);

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
    ) -> Result<Arc<CloneBodyPayloadV1>, String> {
        match self.prior.payload_for(extracted) {
            Some(payload) => {
                self.stats.reused = self.stats.reused.saturating_add(1);
                Ok(Arc::clone(payload))
            }
            None => {
                self.stats.computed = self.stats.computed.saturating_add(1);
                CloneBodyPayloadV1::from_extracted(extracted).map(Arc::new)
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

    fn payload_for(&self, extracted: &ExtractedCloneBodyV1) -> Option<&'a Arc<CloneBodyPayloadV1>> {
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
            rename_tokens: body.rename_tokens.as_ref(),
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
            rename_tokens: payload.rename_tokens.as_ref(),
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
    conservative_tokens: &'a CloneTokenStreamV1,
    tokenization_status: CloneBodyTokenizationStatusV1,
    tokenization_issues: &'a [CloneBodyTokenizationIssueV1],
    rename_revision: Option<u16>,
    rename_tokens: Option<&'a CloneTokenStreamV1>,
    rename_coverage: CloneBodyRenameStatusV1,
    rename_issues: &'a [CloneBodyRenameIssueV1],
}

struct ClonePayloadDigestsV1 {
    body: ManifestDigest,
    conservative: ManifestDigest,
    rename: Option<ManifestDigest>,
    payload: ManifestDigest,
}

/// `canonical_sha256` of the tuple whose elements have the given canonical
/// encodings. Canonical JSON writes a tuple as its elements' own encodings
/// joined into an array, so one token stream serialized once binds every
/// digest that embeds it.
fn canonical_tuple_sha256(elements: &[&[u8]]) -> Result<ManifestDigest, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"[");
    for (index, element) in elements.iter().enumerate() {
        if index > 0 {
            hasher.update(b",");
        }
        hasher.update(element);
    }
    hasher.update(b"]");
    ManifestDigest::from_sha256_bytes(&hasher.finalize()).map_err(|error| error.to_string())
}

fn clone_payload_digests(
    input: ClonePayloadDigestInputV1<'_>,
) -> Result<ClonePayloadDigestsV1, String> {
    fn json<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
        canonical_json_bytes(value).map_err(|error| error.to_string())
    }
    let language = json(&input.language)?;
    let symbol_kind = json(&input.symbol_kind)?;
    let conservative_revision = json(&input.conservative_revision)?;
    let conservative_tokens = json(&input.conservative_tokens)?;
    let rename_tokens = json(&input.rename_tokens)?;
    let body = canonical_tuple_sha256(&[
        &json(&BODY_DIGEST_DOMAIN)?,
        &language,
        &symbol_kind,
        &conservative_revision,
        &conservative_tokens,
    ])?;
    let conservative = canonical_tuple_sha256(&[
        &json(&CONSERVATIVE_DIGEST_DOMAIN)?,
        &language,
        &symbol_kind,
        &conservative_revision,
        &conservative_tokens,
    ])?;
    let rename = if input.rename_coverage == CloneBodyRenameStatusV1::Complete
        && input.tokenization_status == CloneBodyTokenizationStatusV1::Complete
    {
        match (input.rename_revision, input.rename_tokens) {
            (Some(revision), Some(_)) => Some(canonical_tuple_sha256(&[
                &json(&RENAME_DIGEST_DOMAIN)?,
                &language,
                &symbol_kind,
                &json(&revision)?,
                &rename_tokens,
            ])?),
            _ => return Err("complete rename payload is missing tokens or revision".to_owned()),
        }
    } else {
        None
    };
    let payload = canonical_tuple_sha256(&[
        &json(&PAYLOAD_DIGEST_DOMAIN)?,
        &language,
        &symbol_kind,
        &json(&body)?,
        &json(&input.token_count)?,
        &conservative_revision,
        &json(&conservative)?,
        &conservative_tokens,
        &json(&input.tokenization_status)?,
        &json(&input.tokenization_issues)?,
        &json(&input.rename_revision)?,
        &json(&rename)?,
        &rename_tokens,
        &json(&input.rename_coverage)?,
        &json(&input.rename_issues)?,
    ])?;
    Ok(ClonePayloadDigestsV1 {
        body,
        conservative,
        rename,
        payload,
    })
}

impl CodeIndexCloneBodyV1 {
    pub fn retained_owned_bytes(&self) -> usize {
        let conservative = &self.payload.conservative_tokens;
        let rename = self.payload.rename_tokens.as_ref().map_or(0, |rename| {
            let tokens = if rename.shares_tokens_with(conservative) {
                0
            } else {
                rename.token_retained_bytes()
            };
            tokens.saturating_add(rename.rename_retained_bytes())
        });
        conservative
            .token_retained_bytes()
            .saturating_add(rename)
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

/// Everything a clone payload holds except its digests, which are a pure
/// function of these fields.
pub struct CloneBodyPayloadPartsV1 {
    pub language: String,
    pub symbol_kind: String,
    pub token_count: u32,
    pub conservative_normalization_revision: u16,
    pub conservative_tokens: CloneTokenStreamV1,
    pub tokenization_status: CloneBodyTokenizationStatusV1,
    pub tokenization_issues: Vec<CloneBodyTokenizationIssueV1>,
    pub rename_normalization_revision: Option<u16>,
    pub rename_tokens: Option<CloneTokenStreamV1>,
    pub rename_coverage: CloneBodyRenameStatusV1,
    pub rename_issues: Vec<CloneBodyRenameIssueV1>,
}

impl CloneBodyPayloadV1 {
    pub fn from_extracted(body: &ExtractedCloneBodyV1) -> Result<Self, String> {
        Self::from_parts(CloneBodyPayloadPartsV1 {
            language: body.language.clone(),
            symbol_kind: body.symbol_kind.as_str().to_owned(),
            token_count: body.non_trivia_token_count,
            conservative_normalization_revision: body.normalization_revision,
            conservative_tokens: body.conservative_tokens.clone(),
            tokenization_status: body.tokenization_status,
            tokenization_issues: body.tokenization_issues.clone(),
            rename_normalization_revision: body.rename_normalization_revision,
            rename_tokens: body.rename_tokens.clone(),
            rename_coverage: body.rename_status,
            rename_issues: body.rename_issues.clone(),
        })
    }

    pub fn from_parts(parts: CloneBodyPayloadPartsV1) -> Result<Self, String> {
        let digests = clone_payload_digests(ClonePayloadDigestInputV1 {
            language: &parts.language,
            symbol_kind: &parts.symbol_kind,
            token_count: parts.token_count,
            conservative_revision: parts.conservative_normalization_revision,
            conservative_tokens: &parts.conservative_tokens,
            tokenization_status: parts.tokenization_status,
            tokenization_issues: &parts.tokenization_issues,
            rename_revision: parts.rename_normalization_revision,
            rename_tokens: parts.rename_tokens.as_ref(),
            rename_coverage: parts.rename_coverage,
            rename_issues: &parts.rename_issues,
        })?;
        Ok(Self {
            payload_digest: digests.payload,
            language: parts.language,
            symbol_kind: parts.symbol_kind,
            body_digest: digests.body,
            token_count: parts.token_count,
            conservative_normalization_revision: parts.conservative_normalization_revision,
            conservative_digest: digests.conservative,
            conservative_tokens: parts.conservative_tokens,
            tokenization_status: parts.tokenization_status,
            tokenization_issues: parts.tokenization_issues,
            rename_normalization_revision: parts.rename_normalization_revision,
            rename_digest: digests.rename,
            rename_tokens: parts.rename_tokens,
            rename_coverage: parts.rename_coverage,
            rename_issues: parts.rename_issues,
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
            rename_tokens: self.rename_tokens.as_ref(),
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

pub fn verify_exact_clone_payload(
    source: &CloneBodyPayloadV1,
    candidate: &CloneBodyPayloadV1,
    key: &CloneExactKeyV1,
) -> bool {
    fn tokens_for_key<'a>(
        payload: &'a CloneBodyPayloadV1,
        key: &CloneExactKeyV1,
    ) -> Option<&'a CloneTokenStreamV1> {
        match key.class {
            CloneNormalizationClassV1::Conservative
                if payload.conservative_normalization_revision == key.normalization_revision
                    && payload.conservative_digest == key.digest =>
            {
                Some(&payload.conservative_tokens)
            }
            CloneNormalizationClassV1::Rename
                if payload.rename_normalization_revision == Some(key.normalization_revision)
                    && payload.rename_digest.as_ref() == Some(&key.digest) =>
            {
                payload.rename_tokens.as_ref()
            }
            CloneNormalizationClassV1::Conservative | CloneNormalizationClassV1::Rename => None,
        }
    }

    source.validate().is_ok()
        && candidate.validate().is_ok()
        && source.language == candidate.language
        && source.symbol_kind == candidate.symbol_kind
        && matches!(
            (tokens_for_key(source, key), tokens_for_key(candidate, key)),
            (Some(source), Some(candidate)) if source == candidate
        )
}

#[cfg(test)]
mod payload_digest_tests {
    use tracedecay_domain::canonical_sha256;

    use super::{
        BODY_DIGEST_DOMAIN, CONSERVATIVE_DIGEST_DOMAIN, CloneBodyPayloadPartsV1,
        CloneBodyPayloadV1, CloneBodyRenameIssueV1, CloneBodyRenameStatusV1,
        CloneBodyTokenizationIssueV1, CloneBodyTokenizationStatusV1, CloneTokenStreamV1,
        ConservativeCloneTokenV1, PAYLOAD_DIGEST_DOMAIN, RENAME_DIGEST_DOMAIN,
    };

    fn tokens(texts: &[&str]) -> CloneTokenStreamV1 {
        CloneTokenStreamV1::from_tokens(
            std::iter::once(ConservativeCloneTokenV1::StructureStart {
                syntax_kind: "block",
            })
            .chain(texts.iter().map(|text| ConservativeCloneTokenV1::Syntax {
                syntax_kind: "identifier",
                text,
            }))
            .chain(std::iter::once(ConservativeCloneTokenV1::StructureEnd {
                syntax_kind: "block",
            })),
        )
        .expect("stream")
    }

    #[test]
    fn payload_digests_equal_the_canonical_tuple_digests() {
        for rename in [Some(tokens(&["$0", "\"$1\"", "\u{7}"])), None] {
            let complete = rename.is_some();
            let payload = CloneBodyPayloadV1::from_parts(CloneBodyPayloadPartsV1 {
                language: "rust".to_owned(),
                symbol_kind: "function".to_owned(),
                token_count: 5,
                conservative_normalization_revision: 3,
                conservative_tokens: tokens(&["a\"b", "\\c\n", "é\u{1}"]),
                tokenization_status: CloneBodyTokenizationStatusV1::Complete,
                tokenization_issues: if complete {
                    Vec::new()
                } else {
                    vec![CloneBodyTokenizationIssueV1::BodyBoundaryUnavailable]
                },
                rename_normalization_revision: complete.then_some(2),
                rename_tokens: rename.clone(),
                rename_coverage: if complete {
                    CloneBodyRenameStatusV1::Complete
                } else {
                    CloneBodyRenameStatusV1::UnsupportedLanguage
                },
                rename_issues: Vec::<CloneBodyRenameIssueV1>::new(),
            })
            .expect("payload");
            let prefix = (
                payload.language.as_str(),
                payload.symbol_kind.as_str(),
                payload.conservative_normalization_revision,
                &payload.conservative_tokens,
            );
            let body =
                canonical_sha256(&(BODY_DIGEST_DOMAIN, prefix.0, prefix.1, prefix.2, prefix.3))
                    .expect("body");
            let conservative = canonical_sha256(&(
                CONSERVATIVE_DIGEST_DOMAIN,
                prefix.0,
                prefix.1,
                prefix.2,
                prefix.3,
            ))
            .expect("conservative");
            let rename_digest = rename.as_ref().map(|tokens| {
                canonical_sha256(&(RENAME_DIGEST_DOMAIN, prefix.0, prefix.1, 2u16, tokens))
                    .expect("rename")
            });
            let payload_digest = canonical_sha256(&(
                PAYLOAD_DIGEST_DOMAIN,
                prefix.0,
                prefix.1,
                &body,
                payload.token_count,
                prefix.2,
                &conservative,
                prefix.3,
                payload.tokenization_status,
                &payload.tokenization_issues,
                payload.rename_normalization_revision,
                &rename_digest,
                payload.rename_tokens.as_ref(),
                payload.rename_coverage,
                &payload.rename_issues,
            ))
            .expect("payload digest");
            assert_eq!(payload.body_digest, body);
            assert_eq!(payload.conservative_digest, conservative);
            assert_eq!(payload.rename_digest, rename_digest);
            assert_eq!(payload.payload_digest, payload_digest);
            payload.validate().expect("self-consistent payload");
        }
    }
}

#[cfg(test)]
mod exact_verification_tests {
    use tracedecay_code_extraction::{
        CloneBodyEligibilityV1, CloneTokenStreamV1, ConservativeCloneTokenV1, LanguageExtractor,
        TypeScriptExtractor,
    };

    use super::{CloneBodyPayloadV1, CloneNormalizationClassV1, verify_exact_clone_payload};

    #[test]
    fn a_forced_body_digest_collision_cannot_validate_other_token_bytes() {
        let extracted = TypeScriptExtractor.extract_artifact(
            "src/body.ts",
            "function copy(input) { const one = parse(input); const two = use(one); const three = use(two); return finish(three, input, one, two); }",
        );
        let body = extracted.clone_bodies.first().expect("clone body");
        let payload = CloneBodyPayloadV1::from_extracted(body).expect("payload");
        payload
            .validate()
            .expect("extracted tokens match their digests");
        let mut colliding = payload;
        let mut tokens = colliding.conservative_tokens.iter().collect::<Vec<_>>();
        tokens[0] = ConservativeCloneTokenV1::Syntax {
            syntax_kind: "identifier",
            text: "colliding-but-different",
        };
        colliding.conservative_tokens = CloneTokenStreamV1::from_tokens(tokens).expect("stream");
        assert_eq!(
            colliding.validate(),
            Err("clone payload digests do not match their canonical tokens".to_owned())
        );
    }

    #[test]
    fn exact_clone_verification_compares_canonical_token_bytes_after_digest_lookup() {
        let source = TypeScriptExtractor.extract_artifact(
            "src/source.ts",
            "function source(input) { const one = parse(input); const two = use(one); const three = use(two); return finish(three, input, one, two); }",
        );
        let renamed = TypeScriptExtractor.extract_artifact(
            "src/renamed.ts",
            "function renamed(value) { const first = parse(value); const second = use(first); const third = use(second); return finish(third, value, first, second); }",
        );
        let different = TypeScriptExtractor.extract_artifact(
            "src/different.ts",
            "function different(value) { const first = open(value); const second = store(first); const third = store(second); return close(third, value, first, second); }",
        );
        let source = CloneBodyPayloadV1::from_extracted(
            source.clone_bodies.first().expect("source clone body"),
        )
        .expect("source payload");
        let renamed = CloneBodyPayloadV1::from_extracted(
            renamed.clone_bodies.first().expect("renamed clone body"),
        )
        .expect("renamed payload");
        let different = CloneBodyPayloadV1::from_extracted(
            different
                .clone_bodies
                .first()
                .expect("different clone body"),
        )
        .expect("different payload");
        let rename_key = source
            .exact_keys(CloneBodyEligibilityV1::Eligible)
            .into_iter()
            .find(|key| key.class == CloneNormalizationClassV1::Rename)
            .expect("rename key");

        assert!(verify_exact_clone_payload(&source, &renamed, &rename_key));
        assert!(!verify_exact_clone_payload(
            &source,
            &different,
            &rename_key
        ));
    }
}
