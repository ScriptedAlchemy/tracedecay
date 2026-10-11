//! Trims the low-value tail of a ranked result page behind a retrieval handle.
//!
//! Kept rows stay byte-exact, and the cut rows are stored verbatim, so one
//! `tracedecay_retrieve` call restores exactly what the page left out.

use std::path::Path;
use std::time::Instant;

use tracedecay_runtime_core::tracedecay::current_timestamp;
use tracedecay_session_memory::response_handles::response_handle_for;

use super::prepare_truncated_response_handle;
use crate::response_handles::{RESPONSE_RETRIEVE_TOOL, observe_response_truncation};

const MIN_KEPT_ROWS: usize = 3;
const MIN_TRIMMED_ROWS: usize = 2;

/// One rendered row of a ranked result page.
pub(crate) struct RankedRow {
    /// One-based display rank.
    pub rank: u64,
    /// The retrieval kernel's fixed-point utility; integer, so the cut is
    /// exact and repeated calls return identical bytes.
    pub utility_micros: u64,
    /// The row's display name, matched against the query words.
    pub name: String,
    /// A caller `lexical_anchors` route ranked this row, and the anchor
    /// receipt promises its sites are on the page.
    pub anchored: bool,
    /// The exact bytes the untrimmed page emits for this row.
    pub markdown: String,
}

/// True when the name contains every word of the query, compared as
/// lowercase alphanumerics, so `exact_class_rank` and `ExactClass` both
/// answer `exact_class`, and `store_response_handle` answers
/// `store response handle` while a bare `store` field does not.
fn names_every_query_word(name: &str, query: &str) -> bool {
    let fold = |text: &str| -> String {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let name = fold(name);
    let words = query
        .split_whitespace()
        .map(fold)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    !words.is_empty() && words.iter().all(|word| name.contains(word.as_str()))
}

/// Start of the longest suffix of rows that each score at most half the
/// page's best utility and neither carry a caller anchor nor name every
/// query word, or `None` when no worthwhile cut exists.
fn low_value_tail_start(query: &str, rows: &[RankedRow]) -> Option<(usize, u64)> {
    let best = rows
        .iter()
        .map(|row| row.utility_micros)
        .max()
        .filter(|best| *best > 0)?;
    let weak = |row: &RankedRow| {
        !row.anchored
            && u128::from(row.utility_micros) * 2 <= u128::from(best)
            && !names_every_query_word(&row.name, query)
    };
    // Ordering ranks exact tier and source role ahead of utility, so a strong
    // row can follow weak ones; cutting only after the last strong row never
    // drops a strong row and keeps the cut ranks contiguous.
    let start = rows
        .iter()
        .rposition(|row| !weak(row))
        .map_or(0, |last_strong| last_strong + 1)
        .max(MIN_KEPT_ROWS);
    (rows.len().saturating_sub(start) >= MIN_TRIMMED_ROWS).then_some((start, best))
}

/// The rows as the page renders them, with the low-value tail replaced by
/// one marker line naming its retrieval handle. Fails open to every row when
/// the handle cannot be stored.
pub(crate) fn ranked_rows_md(
    response_handle_root: Option<&Path>,
    query: &str,
    rows: &[RankedRow],
) -> String {
    let concat =
        |rows: &[RankedRow]| -> String { rows.iter().map(|row| row.markdown.as_str()).collect() };
    let Some((start, best)) = low_value_tail_start(query, rows) else {
        return concat(rows);
    };
    let started = Instant::now();
    let (kept, cut) = rows.split_at(start);
    let cut_md = concat(cut);
    let marker = trimmed_tail_marker(cut, best, &response_handle_for(&cut_md));
    if marker.len() >= cut_md.len() {
        return concat(rows);
    }
    if prepare_truncated_response_handle(response_handle_root, &cut_md)
        .record
        .is_none()
    {
        return concat(rows);
    }
    let mut rendered = concat(kept);
    let untrimmed_bytes = rendered.len() + cut_md.len();
    rendered.push_str(&marker);
    observe_response_truncation(
        untrimmed_bytes,
        rendered.len(),
        true,
        current_timestamp(),
        "stored",
        started.elapsed(),
    );
    rendered
}

/// Carries no timestamp: the handle is a content digest, so the marker is a
/// pure function of the rows and repeated calls stay prompt-cache identical.
fn trimmed_tail_marker(cut: &[RankedRow], best: u64, handle: &str) -> String {
    let first = cut.first().map_or(0, |row| row.rank);
    let last = cut.last().map_or(0, |row| row.rank);
    let bytes: usize = cut.iter().map(|row| row.markdown.len()).sum();
    format!(
        "- _Trimmed {} lower-ranked results (ranks {first}-{last}, {bytes} bytes): each scored \
         at most half of this page's best utility {best}. To restore them, call \
         `{RESPONSE_RETRIEVE_TOOL}` with handle `{handle}` (pass the same `project_selector` \
         if this call used one)._\n",
        cut.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response_handles::{
        RESPONSE_HANDLE_TTL_SECS, ResponseHandleLookup, retrieve_response_handle,
        store_response_handle,
    };

    fn row(rank: u64, utility_micros: u64, anchored: bool, name: &str) -> RankedRow {
        RankedRow {
            rank,
            utility_micros,
            name: name.to_owned(),
            anchored,
            markdown: format!("- **{name}** rank {rank}\n  anchor_id: `code-symbol:{name}`\n"),
        }
    }

    fn page(utilities: &[u64]) -> Vec<RankedRow> {
        utilities
            .iter()
            .enumerate()
            .map(|(index, utility)| row(index as u64 + 1, *utility, false, &format!("r{index}")))
            .collect()
    }

    /// Two strong rows (one named in non-ASCII) ahead of eight weak ones.
    fn weak_tail_page() -> Vec<RankedRow> {
        let mut rows = page(&[2_000_000, 2_000_000, 2_000_000]);
        rows[1] = row(2, 2_000_000, false, "größe_überprüfen");
        rows.extend((4..=10).map(|rank| row(rank, 1_000_000, false, &format!("weak{rank}"))));
        rows
    }

    fn concat(rows: &[RankedRow]) -> String {
        rows.iter().map(|row| row.markdown.as_str()).collect()
    }

    fn marker_handle(rendered: &str) -> &str {
        let after = rendered
            .split_once("with handle `")
            .expect("marker names a handle")
            .1;
        after.split_once('`').expect("handle is quoted").0
    }

    /// Shared with `scripts/test-measure-ranked-tail-trim.py`, so the
    /// measurement script's copy of the rule answers the same cases.
    #[derive(serde::Deserialize)]
    struct RuleCases {
        tail_start: Vec<TailStartCase>,
        names_every_query_word: Vec<NameCase>,
    }

    #[derive(serde::Deserialize)]
    struct TailStartCase {
        case: String,
        query: String,
        rows: Vec<CaseRow>,
        start: Option<usize>,
    }

    #[derive(serde::Deserialize)]
    struct CaseRow {
        utility: u64,
        name: Option<String>,
        #[serde(default)]
        anchored: bool,
    }

    #[derive(serde::Deserialize)]
    struct NameCase {
        name: String,
        query: String,
        answers: bool,
    }

    fn rule_cases() -> RuleCases {
        serde_json::from_str(include_str!("ranked_tail_cases.json")).expect("rule cases")
    }

    #[test]
    fn tail_start_cuts_only_a_worthwhile_suffix_of_weak_rows() {
        for case in rule_cases().tail_start {
            let rows = case
                .rows
                .iter()
                .enumerate()
                .map(|(index, case_row)| {
                    let name = case_row.name.clone().unwrap_or(format!("r{index}"));
                    row(index as u64 + 1, case_row.utility, case_row.anchored, &name)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                low_value_tail_start(&case.query, &rows).map(|(start, _)| start),
                case.start,
                "{}",
                case.case
            );
        }
    }

    #[test]
    fn a_name_answers_a_query_only_when_it_contains_every_word() {
        for case in rule_cases().names_every_query_word {
            assert_eq!(
                names_every_query_word(&case.name, &case.query),
                case.answers,
                "{} for {:?}",
                case.name,
                case.query
            );
        }
    }

    #[test]
    fn short_weak_tail_stays_untrimmed_without_storing_a_handle() {
        let root = tempfile::tempdir().expect("handle root");
        let rows = page(&[2_000_000, 2_000_000, 2_000_000, 1_000_000, 1_000_000]);
        let untrimmed = concat(&rows);

        assert_eq!(ranked_rows_md(Some(root.path()), "store", &rows), untrimmed);
        assert_eq!(
            std::fs::read_dir(root.path()).expect("handle root").count(),
            0,
            "a marker larger than the omitted rows must not publish a handle"
        );
    }

    #[test]
    fn trimmed_page_keeps_rows_exact_and_restores_the_cut_rows() {
        let root = tempfile::tempdir().expect("handle root");
        let rows = weak_tail_page();
        let kept = concat(&rows[..3]);
        let cut = concat(&rows[3..]);

        let rendered = ranked_rows_md(Some(root.path()), "store", &weak_tail_page());

        let marker = rendered
            .strip_prefix(kept.as_str())
            .expect("kept rows render byte-exact");
        let handle = marker_handle(marker);
        assert_eq!(
            marker,
            format!(
                "- _Trimmed 7 lower-ranked results (ranks 4-10, {} bytes): each scored at most \
                 half of this page's best utility 2000000. To restore them, call \
                 `tracedecay_retrieve` with handle `{handle}` (pass the same `project_selector` \
                 if this call used one)._\n",
                cut.len()
            )
        );
        let ResponseHandleLookup::Found(record) =
            retrieve_response_handle(root.path(), handle, current_timestamp())
                .expect("handle lookup")
        else {
            panic!("the marker's handle restores the cut rows");
        };
        assert_eq!(record.content, cut);
    }

    #[test]
    fn marker_bytes_survive_a_renewed_handle_record() {
        let now = current_timestamp();
        let cut = concat(&weak_tail_page()[3..]);
        let fresh_root = tempfile::tempdir().expect("fresh root");
        let renewed_root = tempfile::tempdir().expect("renewed root");
        // One record keeps most of its lifetime; the other is near expiry, so
        // rendering renews it with a later expiry.
        let handle =
            store_response_handle(fresh_root.path(), &cut, now - RESPONSE_HANDLE_TTL_SECS / 4)
                .expect("seed fresh record")
                .handle;
        store_response_handle(
            renewed_root.path(),
            &cut,
            now - RESPONSE_HANDLE_TTL_SECS + 60,
        )
        .expect("seed expiring record");

        let fresh = ranked_rows_md(Some(fresh_root.path()), "store", &weak_tail_page());
        let renewed = ranked_rows_md(Some(renewed_root.path()), "store", &weak_tail_page());

        let expires_at =
            |root: &Path| match retrieve_response_handle(root, &handle, current_timestamp())
                .expect("handle lookup")
            {
                ResponseHandleLookup::Found(record) => record.expires_at,
                _ => panic!("record is live"),
            };
        assert!(
            expires_at(renewed_root.path()) > expires_at(fresh_root.path()),
            "the arrangement must actually renew one record"
        );
        assert_eq!(fresh, renewed);
    }

    #[test]
    fn unavailable_handle_store_fails_open_to_every_row() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file_root = dir.path().join("not-a-directory");
        std::fs::write(&file_root, b"occupied").expect("regular file");
        let untrimmed = concat(&weak_tail_page());

        assert_eq!(
            ranked_rows_md(Some(&file_root), "store", &weak_tail_page()),
            untrimmed
        );
        assert_eq!(ranked_rows_md(None, "store", &weak_tail_page()), untrimmed);
    }
}
