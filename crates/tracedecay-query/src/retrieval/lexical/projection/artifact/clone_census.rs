use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
use tracedecay_code_index::clones::{
    CloneBodyEligibilityV1, CloneBodyRenameStatusV1, CloneNormalizationClassV1,
};

use super::clone_codec::{decode_clone_eligibility, decode_clone_payload, digest_from_key};
use super::format::decode_fingerprint_postings;
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
    pub excluded_too_large_bodies: u64,
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
) -> Result<HashMap<i64, IncompleteRenameCoverageV1>, CodeLexicalArtifactErrorV1> {
    let mut incomplete_rename = HashMap::new();
    let mut statement = connection
        .prepare(
            "SELECT ordinal, payload_digest, payload FROM clone_body_payloads ORDER BY ordinal",
        )
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let ordinal: i64 = row.get(0).map_err(sqlite_error)?;
        let digest: Vec<u8> = row.get(1).map_err(sqlite_error)?;
        let payload_bytes: Vec<u8> = row.get(2).map_err(sqlite_error)?;
        let payload = decode_clone_payload(&payload_bytes, digest_from_key(&digest)?.as_str())?;
        match payload.rename_coverage {
            CloneBodyRenameStatusV1::Complete => {}
            CloneBodyRenameStatusV1::Partial => {
                incomplete_rename.insert(ordinal, IncompleteRenameCoverageV1::Partial);
            }
            CloneBodyRenameStatusV1::UnsupportedLanguage => {
                incomplete_rename.insert(ordinal, IncompleteRenameCoverageV1::UnsupportedLanguage);
            }
        }
    }
    Ok(incomplete_rename)
}

pub(super) fn read_clone_index_census(
    connection: &Connection,
    hot_posting_threshold: u64,
) -> Result<CodeLexicalCloneIndexCensusV1, CodeLexicalArtifactErrorV1> {
    let mut census = CodeLexicalCloneIndexCensusV1::default();
    let incomplete_rename = validate_stored_clone_payloads(connection)?;
    // The inner join proves every counted occurrence has its verified payload
    // row; the occurrence total below proves none was dropped by it.
    let mut statement = connection
        .prepare(
            "SELECT occurrence.payload_ordinal, occurrence.eligibility
             FROM clone_occurrences AS occurrence
             JOIN clone_body_payloads AS payload
               ON payload.ordinal = occurrence.payload_ordinal
             ORDER BY occurrence.ordinal",
        )
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let payload_ordinal: i64 = row.get(0).map_err(sqlite_error)?;
        let eligibility = row
            .get_ref(1)
            .and_then(|value| value.as_blob().map_err(rusqlite::Error::from))
            .map_err(sqlite_error)?;
        census.source_bodies = census.source_bodies.saturating_add(1);
        match decode_clone_eligibility(eligibility)? {
            CloneBodyEligibilityV1::Eligible => {
                census.eligible_source_bodies = census.eligible_source_bodies.saturating_add(1);
                match incomplete_rename.get(&payload_ordinal) {
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
            CloneBodyEligibilityV1::ExcludedTooLarge { .. } => {
                census.excluded_too_large_bodies =
                    census.excluded_too_large_bodies.saturating_add(1);
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

    let (fingerprint_postings, hot_postings, hot_rows): (i64, i64, i64) = connection
        .query_row(
            "SELECT
               COALESCE(SUM(posting_count), 0),
               COUNT(*) FILTER (WHERE posting_count > ?1),
               COALESCE(SUM(posting_count) FILTER (WHERE posting_count > ?1), 0)
             FROM clone_fingerprint_postings",
            [i64::try_from(hot_posting_threshold)
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(sqlite_error)?;
    // Bodies with positional fingerprints are the distinct occurrences their
    // lists name.
    let mut fingerprinted = HashSet::new();
    let mut statement = connection
        .prepare("SELECT postings FROM clone_fingerprint_postings")
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let encoded = row
            .get_ref(0)
            .and_then(|value| value.as_blob().map_err(rusqlite::Error::from))
            .map_err(sqlite_error)?;
        fingerprinted.extend(
            decode_fingerprint_postings(encoded)?
                .into_iter()
                .map(|(occurrence, _)| occurrence),
        );
    }
    let fingerprint_bodies = i64::try_from(fingerprinted.len())
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    census.near_fingerprint_bodies = count(fingerprint_bodies)?;
    census.near_fingerprint_postings = count(fingerprint_postings)?;
    census.hot_postings = count(hot_postings)?;
    census.hot_posting_rows = count(hot_rows)?;

    Ok(census)
}

#[cfg(test)]
mod tests {
    use super::super::clone_codec::{digest_key, encode_clone_eligibility, encode_clone_payload};
    use super::*;
    use rusqlite::params;
    use std::sync::Arc;
    use tracedecay_code_extraction::{
        CloneBodyTokenizationStatusV1, ConservativeCloneTokenV1, ExtractedCloneBodyV1,
    };
    use tracedecay_code_index::clones::CloneBodyPayloadV1;
    use tracedecay_domain::{NodeKind, SourceSpan};

    /// The tables the census reads. Triggers and the builder gate belong to
    /// the write path, which no census read goes through.
    fn census_schema(connection: &Connection) {
        connection
            .execute_batch(
                "CREATE TABLE clone_body_payloads(
                    ordinal INTEGER PRIMARY KEY,
                    payload_digest BLOB NOT NULL UNIQUE,
                    payload BLOB NOT NULL
                 );
                 CREATE TABLE clone_occurrences(
                    ordinal INTEGER PRIMARY KEY,
                    symbol_key BLOB NOT NULL UNIQUE,
                    payload_ordinal INTEGER NOT NULL,
                    eligibility BLOB NOT NULL
                 );
                 CREATE TABLE clone_exact_postings(
                    class INTEGER NOT NULL,
                    digest BLOB NOT NULL,
                    occurrence_ordinal INTEGER NOT NULL
                 );
                 CREATE TABLE clone_fingerprint_postings(
                    posting_count INTEGER NOT NULL,
                    postings BLOB NOT NULL
                 );",
            )
            .expect("census schema");
    }

    fn payload(seed: usize) -> CloneBodyPayloadV1 {
        let body = ExtractedCloneBodyV1 {
            logical_path: format!("src/body_{seed}.rs"),
            language: "rust".to_owned(),
            symbol_kind: NodeKind::Function,
            symbol_occurrence_id: format!("symbol.body.{seed}"),
            body_span: SourceSpan {
                start_byte: 0,
                end_byte: 64,
            },
            normalization_revision: 1,
            non_trivia_token_count: 8,
            eligibility: CloneBodyEligibilityV1::Eligible,
            tokenization_status: CloneBodyTokenizationStatusV1::Complete,
            tokenization_issues: Vec::new(),
            conservative_tokens: Arc::from(
                (0..8)
                    .map(|index| ConservativeCloneTokenV1::Syntax {
                        syntax_kind: "identifier".into(),
                        text: format!("token_{seed}_{index}"),
                    })
                    .collect::<Vec<_>>(),
            ),
            rename_normalization_revision: None,
            rename_status: CloneBodyRenameStatusV1::UnsupportedLanguage,
            rename_issues: Vec::new(),
            rename_tokens: None,
        };
        CloneBodyPayloadV1::from_extracted(&body).expect("canonical clone payload")
    }

    /// Store `payload` and return its ordinal.
    fn store_payload(connection: &Connection, payload: &CloneBodyPayloadV1) -> i64 {
        connection
            .execute(
                "INSERT INTO clone_body_payloads(payload_digest, payload) VALUES (?1, ?2)",
                params![
                    digest_key(&payload.payload_digest).expect("payload digest key"),
                    encode_clone_payload(payload).expect("payload bytes").0
                ],
            )
            .expect("store payload");
        connection.last_insert_rowid()
    }

    fn store_occurrence(connection: &Connection, id: &str, payload_ordinal: i64) {
        connection
            .execute(
                "INSERT INTO clone_occurrences(symbol_key, payload_ordinal, eligibility)
                 VALUES (?1, ?2, ?3)",
                params![
                    id,
                    payload_ordinal,
                    encode_clone_eligibility(CloneBodyEligibilityV1::Eligible)
                ],
            )
            .expect("store occurrence");
    }

    /// The census joins occurrences to payloads, so an occurrence whose payload
    /// row is absent is invisible to the join. Counting it out of the totals is
    /// an undercount reported as a healthy census, which is worse than a
    /// refusal: the artifact is missing a row the occurrence says it holds.
    #[test]
    fn census_refuses_an_occurrence_whose_payload_row_is_absent() {
        let connection = Connection::open_in_memory().expect("census database");
        census_schema(&connection);
        let present = store_payload(&connection, &payload(0));
        store_occurrence(&connection, "symbol.present", present);
        store_occurrence(&connection, "symbol.absent", present + 1);

        let error = read_clone_index_census(&connection, 8)
            .expect_err("an occurrence without its payload row must refuse the census");
        assert!(
            matches!(error, CodeLexicalArtifactErrorV1::Corrupt(_)),
            "expected a corruption refusal, got {error:?}"
        );
    }

    /// A census over a corpus whose occurrences share few bodies, which is the
    /// shape every real repository has.
    ///
    /// Ignored because it reports a duration rather than asserting one; run it
    /// with `--ignored --nocapture` to re-derive the figure in the module doc.
    #[test]
    #[ignore = "timing measurement, not a pass/fail contract"]
    fn measure_read_clone_index_census() {
        const PAYLOADS: usize = 128;
        const OCCURRENCES_PER_PAYLOAD: usize = 768;

        let connection = Connection::open_in_memory().expect("census database");
        census_schema(&connection);
        for seed in 0..PAYLOADS {
            let ordinal = store_payload(&connection, &payload(seed));
            for index in 0..OCCURRENCES_PER_PAYLOAD {
                store_occurrence(&connection, &format!("symbol.{seed}.{index}"), ordinal);
            }
        }

        let started = std::time::Instant::now();
        let census = read_clone_index_census(&connection, 8).expect("census");
        let elapsed = started.elapsed();
        assert_eq!(
            census.source_bodies,
            (PAYLOADS * OCCURRENCES_PER_PAYLOAD) as u64
        );
        println!(
            "census over {} occurrences / {PAYLOADS} payloads: {:.3}s",
            census.source_bodies,
            elapsed.as_secs_f64()
        );
    }
}
