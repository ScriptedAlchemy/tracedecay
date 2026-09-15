use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension};
use tracedecay_code_index::clones::{
    CLONE_FINGERPRINT_K_V1, CloneBodyEligibilityV1, CloneBodyOccurrenceV1, CloneBodyPayloadV1,
    CloneBodyRenameStatusV1, CloneNormalizationClassV1, ConservativeCloneTokenV1,
    verify_clone_token_anchor,
};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_domain::{ManifestDigest, RetrieverCoverage, SymbolOccurrenceId, canonical_sha256};

use super::format::VerifiedCodeLexicalArtifactV1;
use super::reader::{CloneArtifactCursorPositionV1, CloneArtifactCursorV1, CloneArtifactPageV1};
use super::schema::LexicalArtifactLayoutV1;
use super::{CodeLexicalArtifactErrorV1, sqlite_error};

pub const CLONE_FINGERPRINT_POSTING_ROW_BUDGET_V1: u64 = 16_384;
pub const CLONE_FINGERPRINT_CANDIDATE_BODY_BUDGET_V1: usize = 256;
pub const CLONE_FINGERPRINT_HOT_POSTING_THRESHOLD_V1: u64 = 1_024;
pub const MAX_CLONE_FINGERPRINT_PAGE_BODIES_V1: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneFingerprintStreamDescriptorV1 {
    pub language: String,
    pub class: CloneNormalizationClassV1,
    pub normalization_revision: u16,
    pub rename_tier_unavailable: Option<CloneBodyRenameStatusV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CloneFingerprintArtifactAnchorV1 {
    pub fingerprint: u64,
    pub source_token_position: u32,
    pub candidate_token_position: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneFingerprintArtifactCandidateV1 {
    pub payload: CloneBodyPayloadV1,
    pub occurrences: Vec<CloneBodyOccurrenceV1>,
    pub anchors: Vec<CloneFingerprintArtifactAnchorV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CloneFingerprintPartialReasonV1 {
    PostingRowBudget,
    CandidateBodyBudget,
    HotPostings,
    Cancelled,
    DeadlineExceeded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloneFingerprintCancellationPointV1 {
    FingerprintCountRead,
    PostingRead,
    CandidateVerification,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CloneFingerprintReadAccountingV1 {
    pub posting_rows_examined: u64,
    pub hot_postings_skipped: u64,
    pub hot_posting_rows_skipped: u64,
    pub candidates_admitted: u64,
    pub pairs_verified: u64,
    pub token_work: u64,
    pub elapsed_micros: u64,
    pub cancellation_point: Option<CloneFingerprintCancellationPointV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneFingerprintArtifactReadV1 {
    pub page: CloneArtifactPageV1<CloneFingerprintArtifactCandidateV1>,
    pub stream: Option<CloneFingerprintStreamDescriptorV1>,
    pub source_eligibility: CloneBodyEligibilityV1,
    pub coverage: RetrieverCoverage,
    pub partial_reasons: Vec<CloneFingerprintPartialReasonV1>,
    pub accounting: CloneFingerprintReadAccountingV1,
}

struct CandidateAccumulatorV1 {
    payload: CloneBodyPayloadV1,
    occurrences: BTreeMap<SymbolOccurrenceId, CloneBodyOccurrenceV1>,
    anchors: BTreeSet<CloneFingerprintArtifactAnchorV1>,
    selected_positions: BTreeSet<(u64, u32)>,
}

pub(super) struct CloneFingerprintReadRequestV1<'a> {
    pub(super) layout: LexicalArtifactLayoutV1,
    pub(super) receipt: &'a VerifiedCodeLexicalArtifactV1,
    pub(super) authority_digest: &'a ManifestDigest,
    pub(super) authority: &'a CloneBodyOccurrenceV1,
    pub(super) source: &'a CloneBodyPayloadV1,
    pub(super) cursor: Option<&'a CloneArtifactCursorV1>,
    pub(super) limit: usize,
    pub(super) control: &'a dyn CodeIndexExecutionControlV1,
}

pub(super) fn read_clone_fingerprint_page(
    connection: &Connection,
    request: CloneFingerprintReadRequestV1<'_>,
) -> Result<CloneFingerprintArtifactReadV1, CodeLexicalArtifactErrorV1> {
    let started = Instant::now();
    let CloneFingerprintReadRequestV1 {
        layout,
        receipt,
        authority_digest,
        authority,
        source,
        cursor,
        limit,
        control,
    } = request;
    if !layout.has_clone_fingerprints() {
        return Err(CodeLexicalArtifactErrorV1::Incompatible(
            "clone fingerprint lookup requires lexical artifact revision 16".to_owned(),
        ));
    }
    if limit == 0 || limit > MAX_CLONE_FINGERPRINT_PAGE_BODIES_V1 {
        return Err(CodeLexicalArtifactErrorV1::Contract(format!(
            "clone fingerprint page limit must be within 1..={MAX_CLONE_FINGERPRINT_PAGE_BODIES_V1}"
        )));
    }
    if authority.payload_digest != source.payload_digest || source.validate().is_err() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "clone fingerprint source payload does not match its occurrence".to_owned(),
        ));
    }
    let Some(stream) = source.fingerprint_stream(authority.eligibility) else {
        let accounting = CloneFingerprintReadAccountingV1 {
            elapsed_micros: started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
            ..CloneFingerprintReadAccountingV1::default()
        };
        return Ok(CloneFingerprintArtifactReadV1 {
            page: CloneArtifactPageV1 {
                members: Vec::new(),
                next_cursor: None,
            },
            stream: None,
            source_eligibility: authority.eligibility,
            coverage: RetrieverCoverage {
                examined: 1,
                excluded: 1,
                ..RetrieverCoverage::default()
            },
            partial_reasons: Vec::new(),
            accounting,
        });
    };
    let descriptor = CloneFingerprintStreamDescriptorV1 {
        language: source.language.clone(),
        class: stream.class,
        normalization_revision: stream.normalization_revision,
        rename_tier_unavailable: stream.rename_tier_unavailable,
    };
    let request_digest = canonical_sha256(&(
        "tracedecay.clone-fingerprint-request.v1",
        receipt.artifact_digest(),
        authority_digest,
        &source.payload_digest,
        &source.body_digest,
        &descriptor.language,
        descriptor.class,
        descriptor.normalization_revision,
    ))
    .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    let after = match cursor {
        Some(cursor)
            if cursor.artifact_digest == *receipt.artifact_digest()
                && cursor.generation == *receipt.generation()
                && cursor.request_digest == request_digest =>
        {
            match &cursor.after {
                CloneArtifactCursorPositionV1::Fingerprint {
                    body_digest,
                    payload_digest,
                } => Some((body_digest.as_str(), payload_digest.as_str())),
                CloneArtifactCursorPositionV1::Exact(_) => {
                    return Err(CodeLexicalArtifactErrorV1::Contract(
                        "clone cursor position does not match a fingerprint read".to_owned(),
                    ));
                }
            }
        }
        Some(_) => {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "clone fingerprint cursor does not match its artifact, generation, or request"
                    .to_owned(),
            ));
        }
        None => None,
    };

    let source_positions = source
        .fingerprint_positions(authority.eligibility)
        .map_err(CodeLexicalArtifactErrorV1::Contract)?;
    if source_positions.is_empty() {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "eligible clone body has no winnowed fingerprints".to_owned(),
        ));
    }
    let mut positions_by_fingerprint = BTreeMap::<u64, Vec<u32>>::new();
    for position in source_positions {
        positions_by_fingerprint
            .entry(position.fingerprint)
            .or_default()
            .push(position.token_position);
    }
    let mut accounting = CloneFingerprintReadAccountingV1 {
        token_work: u64::try_from(stream.tokens.len()).map_err(contract_number)?,
        ..CloneFingerprintReadAccountingV1::default()
    };
    let mut partial_reasons = BTreeSet::new();
    let mut ordered_lists = Vec::with_capacity(positions_by_fingerprint.len());
    let mut count_statement = connection
        .prepare_cached(
            "SELECT posting_count FROM clone_fingerprint_counts WHERE language = ?1 AND class = ?2 AND normalization_revision = ?3 AND fingerprint = ?4",
        )
        .map_err(sqlite_error)?;
    for fingerprint in positions_by_fingerprint.keys().copied() {
        if interrupt(
            control,
            CloneFingerprintCancellationPointV1::FingerprintCountRead,
            &mut accounting,
            &mut partial_reasons,
        ) {
            break;
        }
        let count: Option<i64> = count_statement
            .query_row(
                rusqlite::params![
                    descriptor.language,
                    i64::from(descriptor.class as u8),
                    i64::from(descriptor.normalization_revision),
                    i64::try_from(fingerprint).map_err(contract_number)?,
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        let count = count.ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "clone fingerprint posting is missing its stored count".to_owned(),
            )
        })?;
        ordered_lists.push((
            u64::try_from(count).map_err(|_| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "clone fingerprint posting count is negative".to_owned(),
                )
            })?,
            fingerprint,
        ));
    }
    ordered_lists.sort_unstable();

    let mut candidates =
        BTreeMap::<(ManifestDigest, ManifestDigest), CandidateAccumulatorV1>::new();
    let mut stop = accounting.cancellation_point.is_some();
    for (posting_count, fingerprint) in ordered_lists {
        if stop {
            break;
        }
        if posting_count > CLONE_FINGERPRINT_HOT_POSTING_THRESHOLD_V1 {
            accounting.hot_postings_skipped = accounting.hot_postings_skipped.saturating_add(1);
            accounting.hot_posting_rows_skipped = accounting
                .hot_posting_rows_skipped
                .saturating_add(posting_count);
            partial_reasons.insert(CloneFingerprintPartialReasonV1::HotPostings);
            continue;
        }
        let remaining = CLONE_FINGERPRINT_POSTING_ROW_BUDGET_V1 - accounting.posting_rows_examined;
        if remaining == 0 {
            partial_reasons.insert(CloneFingerprintPartialReasonV1::PostingRowBudget);
            break;
        }
        let mut statement = connection
            .prepare_cached(
                "SELECT posting.symbol_occurrence_id, posting.token_position, posting.payload_digest, posting.body_digest, occurrence.occurrence, payload.payload
                 FROM clone_fingerprint_postings AS posting
                 LEFT JOIN clone_occurrences AS occurrence ON occurrence.symbol_occurrence_id = posting.symbol_occurrence_id
                 LEFT JOIN clone_body_payloads AS payload ON payload.payload_digest = posting.payload_digest
                 WHERE posting.language = ?1 AND posting.class = ?2 AND posting.normalization_revision = ?3 AND posting.fingerprint = ?4
                 ORDER BY posting.symbol_occurrence_id, posting.token_position
                 LIMIT ?5",
            )
            .map_err(sqlite_error)?;
        let read_limit = remaining.min(posting_count);
        let mut rows = statement
            .query(rusqlite::params![
                descriptor.language,
                i64::from(descriptor.class as u8),
                i64::from(descriptor.normalization_revision),
                i64::try_from(fingerprint).map_err(contract_number)?,
                i64::try_from(read_limit).map_err(contract_number)?,
            ])
            .map_err(sqlite_error)?;
        while let Some(row) = rows.next().map_err(sqlite_error)? {
            accounting.posting_rows_examined = accounting.posting_rows_examined.saturating_add(1);
            if interrupt(
                control,
                CloneFingerprintCancellationPointV1::PostingRead,
                &mut accounting,
                &mut partial_reasons,
            ) {
                stop = true;
                break;
            }
            let posting_occurrence: String = row.get(0).map_err(sqlite_error)?;
            let candidate_position = u32::try_from(row.get::<_, i64>(1).map_err(sqlite_error)?)
                .map_err(|_| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "clone fingerprint token position is outside u32".to_owned(),
                    )
                })?;
            let posting_payload: String = row.get(2).map_err(sqlite_error)?;
            let posting_body: String = row.get(3).map_err(sqlite_error)?;
            let occurrence_bytes: Option<Vec<u8>> = row.get(4).map_err(sqlite_error)?;
            let payload_bytes: Option<Vec<u8>> = row.get(5).map_err(sqlite_error)?;
            let (Some(occurrence_bytes), Some(payload_bytes)) = (occurrence_bytes, payload_bytes)
            else {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "clone fingerprint posting is missing its occurrence or payload".to_owned(),
                ));
            };
            let occurrence: CloneBodyOccurrenceV1 = serde_json::from_slice(&occurrence_bytes)
                .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
            let payload: CloneBodyPayloadV1 = serde_json::from_slice(&payload_bytes)
                .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
            if occurrence.symbol_occurrence_id.as_str() != posting_occurrence
                || occurrence.project_id != authority.project_id
                || occurrence.repository_id != authority.repository_id
                || occurrence.worktree_id != authority.worktree_id
                || occurrence.source_generation != *receipt.generation()
                || occurrence.snapshot_digest != authority.snapshot_digest
                || occurrence.payload_digest.as_str() != posting_payload
                || occurrence.payload_digest != payload.payload_digest
                || payload.body_digest.as_str() != posting_body
                || payload.validate().is_err()
            {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "clone fingerprint posting does not match its payload and occurrence"
                        .to_owned(),
                ));
            }
            if occurrence.symbol_occurrence_id == authority.symbol_occurrence_id
                || payload.language != source.language
                || payload.symbol_kind != source.symbol_kind
                || !candidate_size_ratio_admitted(source.token_count, payload.token_count)
            {
                continue;
            }
            let Some(candidate_stream) = payload.fingerprint_stream(occurrence.eligibility) else {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "clone fingerprint posting refers to an excluded payload".to_owned(),
                ));
            };
            if candidate_stream.class != descriptor.class
                || candidate_stream.normalization_revision != descriptor.normalization_revision
            {
                continue;
            }
            let key = (payload.body_digest.clone(), payload.payload_digest.clone());
            if !candidates.contains_key(&key) {
                if candidates.len() == CLONE_FINGERPRINT_CANDIDATE_BODY_BUDGET_V1 {
                    partial_reasons.insert(CloneFingerprintPartialReasonV1::CandidateBodyBudget);
                    stop = true;
                    break;
                }
                let selected_positions = payload
                    .fingerprint_positions(occurrence.eligibility)
                    .map_err(CodeLexicalArtifactErrorV1::Contract)?
                    .into_iter()
                    .map(|position| (position.fingerprint, position.token_position))
                    .collect();
                accounting.candidates_admitted = accounting.candidates_admitted.saturating_add(1);
                accounting.token_work = accounting.token_work.saturating_add(
                    u64::try_from(candidate_stream.tokens.len()).map_err(contract_number)?,
                );
                candidates.insert(
                    key.clone(),
                    CandidateAccumulatorV1 {
                        payload,
                        occurrences: BTreeMap::new(),
                        anchors: BTreeSet::new(),
                        selected_positions,
                    },
                );
            }
            let candidate = candidates.get_mut(&key).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "admitted clone fingerprint candidate disappeared".to_owned(),
                )
            })?;
            if !candidate
                .selected_positions
                .contains(&(fingerprint, candidate_position))
            {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "clone fingerprint posting is not selected by its canonical payload".to_owned(),
                ));
            }
            if interrupt(
                control,
                CloneFingerprintCancellationPointV1::CandidateVerification,
                &mut accounting,
                &mut partial_reasons,
            ) {
                stop = true;
                break;
            }
            let source_positions = positions_by_fingerprint.get(&fingerprint).ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "clone fingerprint source position is unavailable".to_owned(),
                )
            })?;
            let candidate_tokens = fingerprint_tokens(&candidate.payload, descriptor.class)?;
            let mut verified = None;
            for source_position in source_positions {
                accounting.token_work = accounting.token_work.saturating_add(
                    u64::try_from(CLONE_FINGERPRINT_K_V1).map_err(contract_number)?,
                );
                if verify_clone_token_anchor(
                    stream.tokens,
                    *source_position,
                    candidate_tokens,
                    candidate_position,
                ) {
                    verified = Some(CloneFingerprintArtifactAnchorV1 {
                        fingerprint,
                        source_token_position: *source_position,
                        candidate_token_position: candidate_position,
                    });
                    break;
                }
            }
            if let Some(anchor) = verified {
                if candidate.anchors.is_empty() {
                    accounting.pairs_verified = accounting.pairs_verified.saturating_add(1);
                }
                candidate.anchors.insert(anchor);
                candidate
                    .occurrences
                    .insert(occurrence.symbol_occurrence_id.clone(), occurrence);
            }
        }
        if read_limit < posting_count {
            partial_reasons.insert(CloneFingerprintPartialReasonV1::PostingRowBudget);
            break;
        }
    }

    let mut candidates = candidates
        .into_iter()
        .filter(|(_, candidate)| !candidate.anchors.is_empty())
        .filter(|((body_digest, payload_digest), _)| {
            after.is_none_or(|after| (body_digest.as_str(), payload_digest.as_str()) > after)
        })
        .map(|(_, candidate)| CloneFingerprintArtifactCandidateV1 {
            payload: candidate.payload,
            occurrences: candidate.occurrences.into_values().collect(),
            anchors: candidate.anchors.into_iter().collect(),
        })
        .collect::<Vec<_>>();
    let has_more = candidates.len() > limit;
    candidates.truncate(limit);
    let next_cursor = if has_more {
        candidates.last().map(|candidate| CloneArtifactCursorV1 {
            artifact_digest: receipt.artifact_digest().clone(),
            generation: receipt.generation().clone(),
            request_digest,
            after: CloneArtifactCursorPositionV1::Fingerprint {
                body_digest: candidate.payload.body_digest.clone(),
                payload_digest: candidate.payload.payload_digest.clone(),
            },
        })
    } else {
        None
    };
    let interrupted = accounting.cancellation_point.is_some();
    let coverage = RetrieverCoverage {
        examined: 1,
        eligible: 1,
        capped: u64::from(!partial_reasons.is_empty() && !interrupted),
        unknown: u64::from(interrupted),
        ..RetrieverCoverage::default()
    };
    accounting.elapsed_micros = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    Ok(CloneFingerprintArtifactReadV1 {
        page: CloneArtifactPageV1 {
            members: candidates,
            next_cursor,
        },
        stream: Some(descriptor),
        source_eligibility: authority.eligibility,
        coverage,
        partial_reasons: partial_reasons.into_iter().collect(),
        accounting,
    })
}

fn fingerprint_tokens(
    payload: &CloneBodyPayloadV1,
    class: CloneNormalizationClassV1,
) -> Result<&[ConservativeCloneTokenV1], CodeLexicalArtifactErrorV1> {
    match class {
        CloneNormalizationClassV1::Conservative => Ok(&payload.conservative_tokens),
        CloneNormalizationClassV1::Rename => payload.rename_tokens.as_deref().ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "rename fingerprint payload is missing canonical tokens".to_owned(),
            )
        }),
    }
}

fn candidate_size_ratio_admitted(left: u32, right: u32) -> bool {
    let minimum = u64::from(left.min(right));
    let maximum = u64::from(left.max(right));
    minimum.saturating_mul(100) >= maximum.saturating_mul(70)
}

fn interrupt(
    control: &dyn CodeIndexExecutionControlV1,
    point: CloneFingerprintCancellationPointV1,
    accounting: &mut CloneFingerprintReadAccountingV1,
    reasons: &mut BTreeSet<CloneFingerprintPartialReasonV1>,
) -> bool {
    let reason = if control.is_cancelled() {
        Some(CloneFingerprintPartialReasonV1::Cancelled)
    } else if control.is_deadline_exceeded() {
        Some(CloneFingerprintPartialReasonV1::DeadlineExceeded)
    } else {
        None
    };
    if let Some(reason) = reason {
        accounting.cancellation_point = Some(point);
        reasons.insert(reason);
        true
    } else {
        false
    }
}

fn contract_number(error: impl std::fmt::Display) -> CodeLexicalArtifactErrorV1 {
    CodeLexicalArtifactErrorV1::Contract(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::candidate_size_ratio_admitted;

    #[test]
    fn whole_body_size_ratio_has_an_exact_seventy_percent_boundary() {
        assert!(candidate_size_ratio_admitted(70, 100));
        assert!(candidate_size_ratio_admitted(100, 70));
        assert!(!candidate_size_ratio_admitted(69, 100));
    }
}
