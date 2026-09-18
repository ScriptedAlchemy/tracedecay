use std::collections::HashMap;

use rusqlite::Connection;
use tracedecay_code_index::clones::{
    CloneBodyEligibilityV1, CloneBodyOccurrenceV1, CloneBodyPayloadV1, CloneBodyRenameStatusV1,
    CloneNormalizationClassV1,
};

use super::{CodeLexicalArtifactErrorV1, sqlite_error};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeLexicalCloneIndexCensusV1 {
    pub source_bodies: u64,
    pub eligible_source_bodies: u64,
    pub conservative_normalized_bodies: u64,
    pub rename_normalized_bodies: u64,
    pub unique_payloads: u64,
    pub exact_postings: u64,
    pub near_fingerprint_bodies: u64,
    pub near_fingerprint_postings: u64,
    pub hot_postings: u64,
    pub hot_posting_rows: u64,
    pub excluded_too_small_bodies: u64,
    pub excluded_incomplete_tokenization_bodies: u64,
    pub rename_partial_bodies: u64,
    pub rename_unsupported_bodies: u64,
}

/// The rename coverages the per-occurrence counters distinguish. `Complete`
/// bumps no counter, so it is never retained and the census cannot hold a
/// coverage it would then have to drop.
#[derive(Clone, Copy)]
enum IncompleteRenameCoverageV1 {
    Partial,
    UnsupportedLanguage,
}

/// Validate every stored clone payload once and report which of them lack
/// complete rename normalization.
///
/// `clone_body_payloads` is keyed by payload digest, so one row backs every
/// occurrence that shares that body. Re-deriving a payload's four canonical
/// digests once per occurrence therefore repeated the same verification for
/// every duplicate: a generated 768-file corpus stores 98,304 occurrences over
/// 128 distinct payloads, and the census spent ~1.2 s of single-threaded
/// verification where 128 validations were the whole obligation.
///
/// Only the non-`Complete` coverages are retained, because those are the only
/// ones the per-occurrence rename counters distinguish.
fn validate_stored_clone_payloads(
    connection: &Connection,
) -> Result<HashMap<String, IncompleteRenameCoverageV1>, CodeLexicalArtifactErrorV1> {
    let mut incomplete_rename = HashMap::new();
    let mut statement = connection
        .prepare("SELECT payload_digest, payload FROM clone_body_payloads ORDER BY payload_digest")
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let digest: String = row.get(0).map_err(sqlite_error)?;
        let payload_bytes: Vec<u8> = row.get(1).map_err(sqlite_error)?;
        let payload: CloneBodyPayloadV1 = serde_json::from_slice(&payload_bytes)
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        if payload.payload_digest.as_str() != digest || payload.validate().is_err() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "clone census found a payload outside its stored digest".to_owned(),
            ));
        }
        match payload.rename_coverage {
            CloneBodyRenameStatusV1::Complete => {}
            CloneBodyRenameStatusV1::Partial => {
                incomplete_rename.insert(digest, IncompleteRenameCoverageV1::Partial);
            }
            CloneBodyRenameStatusV1::UnsupportedLanguage => {
                incomplete_rename.insert(digest, IncompleteRenameCoverageV1::UnsupportedLanguage);
            }
        }
    }
    Ok(incomplete_rename)
}

pub(super) fn read_clone_index_census(
    connection: &Connection,
    has_fingerprints: bool,
    hot_posting_threshold: u64,
) -> Result<CodeLexicalCloneIndexCensusV1, CodeLexicalArtifactErrorV1> {
    let mut census = CodeLexicalCloneIndexCensusV1::default();
    let incomplete_rename = validate_stored_clone_payloads(connection)?;
    // The inner join proves every counted occurrence has its verified payload
    // row; the occurrence total below proves none was dropped by it.
    let mut statement = connection
        .prepare(
            "SELECT occurrence.payload_digest, occurrence.occurrence
             FROM clone_occurrences AS occurrence
             JOIN clone_body_payloads AS payload
               ON payload.payload_digest = occurrence.payload_digest
             ORDER BY occurrence.symbol_occurrence_id",
        )
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let digest: String = row.get(0).map_err(sqlite_error)?;
        let occurrence_bytes: Vec<u8> = row.get(1).map_err(sqlite_error)?;
        let occurrence: CloneBodyOccurrenceV1 = serde_json::from_slice(&occurrence_bytes)
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        if occurrence.payload_digest.as_str() != digest {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "clone census found a payload outside its occurrence binding".to_owned(),
            ));
        }
        census.source_bodies = census.source_bodies.saturating_add(1);
        match occurrence.eligibility {
            CloneBodyEligibilityV1::Eligible => {
                census.eligible_source_bodies = census.eligible_source_bodies.saturating_add(1);
                match incomplete_rename.get(&digest) {
                    None => {}
                    Some(IncompleteRenameCoverageV1::Partial) => {
                        census.rename_partial_bodies =
                            census.rename_partial_bodies.saturating_add(1);
                    }
                    Some(IncompleteRenameCoverageV1::UnsupportedLanguage) => {
                        census.rename_unsupported_bodies =
                            census.rename_unsupported_bodies.saturating_add(1);
                    }
                }
            }
            CloneBodyEligibilityV1::ExcludedIncompleteTokenization => {
                census.excluded_incomplete_tokenization_bodies = census
                    .excluded_incomplete_tokenization_bodies
                    .saturating_add(1);
            }
            CloneBodyEligibilityV1::ExcludedTooSmall { .. } => {
                census.excluded_too_small_bodies =
                    census.excluded_too_small_bodies.saturating_add(1);
            }
        }
    }
    drop(rows);
    drop(statement);

    let (unique_payloads, exact_postings, conservative, rename, occurrences): (
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = connection
        .query_row(
            "SELECT
                   (SELECT COUNT(*) FROM clone_body_payloads),
                   (SELECT COUNT(*) FROM clone_exact_postings),
                   (SELECT COUNT(*) FROM clone_exact_postings WHERE class = ?1),
                   (SELECT COUNT(*) FROM clone_exact_postings WHERE class = ?2),
                   (SELECT COUNT(*) FROM clone_occurrences)",
            [
                i64::from(CloneNormalizationClassV1::Conservative as u8),
                i64::from(CloneNormalizationClassV1::Rename as u8),
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(sqlite_error)?;
    let count = |value: i64| -> Result<u64, CodeLexicalArtifactErrorV1> {
        u64::try_from(value).map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))
    };
    if count(occurrences)? != census.source_bodies {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "clone census found an occurrence without its payload".to_owned(),
        ));
    }
    census.unique_payloads = count(unique_payloads)?;
    census.exact_postings = count(exact_postings)?;
    census.conservative_normalized_bodies = count(conservative)?;
    census.rename_normalized_bodies = count(rename)?;

    if has_fingerprints {
        let (fingerprint_bodies, fingerprint_postings, hot_postings, hot_rows): (
            i64,
            i64,
            i64,
            i64,
        ) = connection
            .query_row(
                "SELECT
                   (SELECT COUNT(DISTINCT symbol_occurrence_id) FROM clone_fingerprint_postings),
                   (SELECT COUNT(*) FROM clone_fingerprint_postings),
                   (SELECT COUNT(*) FROM clone_fingerprint_counts WHERE posting_count > ?1),
                   (SELECT COALESCE(SUM(posting_count), 0) FROM clone_fingerprint_counts WHERE posting_count > ?1)",
                [i64::try_from(hot_posting_threshold).map_err(|error| {
                    CodeLexicalArtifactErrorV1::Contract(error.to_string())
                })?],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(sqlite_error)?;
        census.near_fingerprint_bodies = count(fingerprint_bodies)?;
        census.near_fingerprint_postings = count(fingerprint_postings)?;
        census.hot_postings = count(hot_postings)?;
        census.hot_posting_rows = count(hot_rows)?;
    }
    Ok(census)
}
