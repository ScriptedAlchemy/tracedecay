//! Carry the parent's clone rows: replace the replaced files' occurrences in
//! place and renumber payloads by first use, as a cold build's insert order
//! numbers them.

use std::collections::BTreeMap;

use rusqlite::types::Value;
use rusqlite::{Transaction, params};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;

use super::super::builder::stored_digest_key;
use super::super::clone_codec::digest_key;
use super::super::format::contract_number;
use super::super::prepared::PreparedCloneBodyV1;
use super::super::row_codec::stored_symbol_key;
use super::super::{CodeLexicalArtifactErrorV1, checkpoint, sqlite_error};
use super::postings::scan_window;
use super::{ReplacedFilesV1, ShiftV1, shift_rowid_keys, shifted};

/// One successor clone body with the occurrence ordinal it takes.
struct CarriedCloneV1<'a> {
    ordinal: i64,
    body: &'a PreparedCloneBodyV1,
}

/// Replace the replaced files' clone occurrences in place, then renumber the
/// payloads by first use over the whole occurrence sequence, which is how a
/// cold build's insert order numbers them.
pub(super) fn carry_clone_rows(
    transaction: &Transaction<'_>,
    clones: &ShiftV1,
    changed: &[ReplacedFilesV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut bodies = Vec::new();
    for (index, file) in changed.iter().enumerate() {
        let first = clone_range_image(clones, index, file)?;
        for (offset, body) in file
            .pages
            .iter()
            .flat_map(|page| &page.clone_bodies)
            .enumerate()
        {
            bodies.push(CarriedCloneV1 {
                ordinal: i64::try_from(first + offset as u64).map_err(contract_number)?,
                body,
            });
        }
    }
    let replaced = clones.sql_replaced("occurrence_ordinal");
    transaction
        .execute(
            &format!("DELETE FROM clone_exact_postings WHERE {replaced}"),
            [],
        )
        .map_err(sqlite_error)?;
    // A TEMP copy of every surviving exact posting retains unlinked pager pages
    // after DROP until this long-lived builder resets its TEMP schema. Move the
    // composite primary keys through bounded, ordered windows and a negative
    // ordinal namespace. A single bulk UPDATE can itself spill an unlinked
    // statement-local key index, even when the TEMP pager has zero pages.
    shift_clone_posting_keys(transaction, clones, control)?;
    {
        let mut insert = transaction
            .prepare_cached(
                "INSERT INTO clone_exact_postings(class, normalization_revision, digest, occurrence_ordinal) VALUES (?1, ?2, ?3, ?4)",
            )
            .map_err(sqlite_error)?;
        for carried in &bodies {
            for key in &carried.body.exact_keys {
                checkpoint(control)?;
                insert
                    .execute(params![
                        i64::from(key.class as u8),
                        i64::from(key.normalization_revision),
                        digest_key(&key.digest)?.as_slice(),
                        carried.ordinal
                    ])
                    .map_err(sqlite_error)?;
            }
        }
    }
    transaction
        .execute(
            &format!(
                "DELETE FROM clone_occurrences WHERE {}",
                clones.sql_replaced("ordinal")
            ),
            [],
        )
        .map_err(sqlite_error)?;
    shift_rowid_keys(transaction, "clone_occurrences", "ordinal", clones, control)?;
    let mut parent_payloads = BTreeMap::<i64, Vec<u8>>::new();
    {
        let mut statement = transaction
            .prepare("SELECT ordinal, payload_digest FROM clone_body_payloads ORDER BY ordinal")
            .map_err(sqlite_error)?;
        let mut rows = statement.query([]).map_err(sqlite_error)?;
        while let Some(row) = rows.next().map_err(sqlite_error)? {
            parent_payloads.insert(
                row.get(0).map_err(sqlite_error)?,
                row.get(1).map_err(sqlite_error)?,
            );
        }
    }
    let mut successor_payloads = BTreeMap::<i64, ([u8; 32], &[u8])>::new();
    {
        let mut insert = transaction
            .prepare_cached(
                "INSERT INTO clone_occurrences(ordinal, symbol_key, payload_ordinal, path, body_start, body_end, eligibility) VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6)",
            )
            .map_err(sqlite_error)?;
        for carried in &bodies {
            checkpoint(control)?;
            let body = carried.body;
            insert
                .execute(params![
                    carried.ordinal,
                    stored_symbol_key(&body.symbol_occurrence_id),
                    body.path,
                    i64::try_from(body.body_start).map_err(contract_number)?,
                    i64::try_from(body.body_end).map_err(contract_number)?,
                    body.eligibility,
                ])
                .map_err(sqlite_error)?;
            successor_payloads.insert(
                carried.ordinal,
                (
                    stored_digest_key(&body.payload_digest)?,
                    body.payload.as_slice(),
                ),
            );
        }
    }
    renumber_clone_payloads(transaction, &parent_payloads, &successor_payloads, control)
}

/// A key-ordered window, so SQLite can seek the WITHOUT ROWID primary key
/// without sorting the entire posting index into an external TEMP file.
type ClonePostingKey = (i64, i64, Vec<u8>, i64);

fn clone_posting_window(
    transaction: &Transaction<'_>,
    predicate: &str,
    last: &Option<ClonePostingKey>,
) -> Result<Vec<ClonePostingKey>, CodeLexicalArtifactErrorV1> {
    let columns = "class, normalization_revision, digest, occurrence_ordinal";
    scan_window(
        transaction,
        &format!(
            "SELECT {columns} FROM clone_exact_postings WHERE {predicate} ORDER BY {columns} LIMIT ?1"
        ),
        &format!(
            "SELECT {columns} FROM clone_exact_postings WHERE {predicate} AND ({columns}) > (?1, ?2, ?3, ?4) ORDER BY {columns} LIMIT ?5"
        ),
        last,
        |(class, revision, digest, ordinal)| {
            vec![
                Value::Integer(*class),
                Value::Integer(*revision),
                Value::Blob(digest.clone()),
                Value::Integer(*ordinal),
            ]
        },
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
}

/// Shift the occurrence component of the WITHOUT ROWID posting primary key.
/// Both passes seek bounded primary-key windows and update one keyed row at a
/// time: SQLite's single bulk UPDATE can allocate an unlinked statement-local
/// key index independently of the TEMP b-tree pager. The negative namespace
/// prevents collisions with positive keys while a window advances.
fn shift_clone_posting_keys(
    transaction: &Transaction<'_>,
    clones: &ShiftV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let Some(first) = clones.first_affected().filter(|_| clones.shifts()) else {
        return Ok(());
    };
    let mut update = transaction
        .prepare_cached(
            "UPDATE clone_exact_postings SET occurrence_ordinal = ?5
             WHERE class = ?1 AND normalization_revision = ?2
               AND digest = ?3 AND occurrence_ordinal = ?4",
        )
        .map_err(sqlite_error)?;
    let mut last = None;
    loop {
        checkpoint(control)?;
        let window = clone_posting_window(
            transaction,
            &format!("occurrence_ordinal >= {first}"),
            &last,
        )?;
        let Some(final_key) = window.last() else {
            break;
        };
        last = Some(final_key.clone());
        for (class, revision, digest, ordinal) in window {
            checkpoint(control)?;
            let mapped = clones
                .map(u64::try_from(ordinal).map_err(contract_number)?)?
                .ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "carried clone posting belongs to a replaced range".to_owned(),
                    )
                })?;
            let mapped = i64::try_from(mapped).map_err(contract_number)?;
            // -mapped - 1 fits even when mapped == i64::MAX.
            let staged = -mapped - 1;
            if update
                .execute(params![class, revision, digest, ordinal, staged])
                .map_err(sqlite_error)?
                != 1
            {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "carried clone posting changed during key shift".to_owned(),
                ));
            }
        }
    }
    // All moved keys are now negative and disjoint from every unchanged key.
    // Keyset pagination remains stable as restored positive keys leave the
    // scan; compute -(ordinal + 1) to handle an i64::MIN staged key.
    let mut last = None;
    loop {
        checkpoint(control)?;
        let window = clone_posting_window(transaction, "occurrence_ordinal < 0", &last)?;
        let Some(final_key) = window.last() else {
            break;
        };
        last = Some(final_key.clone());
        for (class, revision, digest, ordinal) in window {
            checkpoint(control)?;
            let restored = -(ordinal + 1);
            if update
                .execute(params![class, revision, digest, ordinal, restored])
                .map_err(sqlite_error)?
                != 1
            {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "staged clone posting changed during key restoration".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

/// Where a run's first successor occurrence lands: its parent start moved
/// by the growth of the runs before it. `index` pairs the run with its
/// replaced range, since two runs without clone bodies can replace the same
/// empty range.
fn clone_range_image(
    clones: &ShiftV1,
    index: usize,
    file: &ReplacedFilesV1,
) -> Result<u64, CodeLexicalArtifactErrorV1> {
    let start = file.old_clones.0 + 1;
    if clones.replaced.get(index) != Some(&(start, file.old_clones.1 + 1)) {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "carried clone range is missing".to_owned(),
        ));
    }
    let growth_before = index
        .checked_sub(1)
        .map_or(0, |previous| clones.growth[previous]);
    shifted(start, growth_before)
}

/// Restore staged negative payload ordinals without the statement-local
/// key index that a bulk INTEGER PRIMARY KEY UPDATE can create. Restored
/// positive keys cannot re-enter the negative-key scan.
fn restore_clone_payload_ordinals(
    transaction: &Transaction<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut update = transaction
        .prepare_cached("UPDATE clone_body_payloads SET ordinal = ?2 WHERE ordinal = ?1")
        .map_err(sqlite_error)?;
    let mut last: Option<i64> = None;
    loop {
        checkpoint(control)?;
        let window: Vec<i64> = scan_window(
            transaction,
            "SELECT ordinal FROM clone_body_payloads WHERE ordinal < 0 ORDER BY ordinal LIMIT ?1",
            "SELECT ordinal FROM clone_body_payloads WHERE ordinal < 0 AND ordinal > ?1 ORDER BY ordinal LIMIT ?2",
            &last,
            |ordinal| vec![Value::Integer(*ordinal)],
            |row| row.get(0),
        )?;
        let Some(final_ordinal) = window.last() else {
            break;
        };
        last = Some(*final_ordinal);
        for ordinal in window {
            checkpoint(control)?;
            // Reversing -number-1 as -(ordinal+1) also handles i64::MIN.
            let restored = -(ordinal + 1);
            if update
                .execute(params![ordinal, restored])
                .map_err(sqlite_error)?
                != 1
            {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "staged clone payload changed during key restoration".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

/// Number payloads by first use over the occurrence sequence, retire the
/// ones no occurrence uses, and point every occurrence at its number.
fn renumber_clone_payloads(
    transaction: &Transaction<'_>,
    parent_payloads: &BTreeMap<i64, Vec<u8>>,
    successor_payloads: &BTreeMap<i64, ([u8; 32], &[u8])>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let parent_by_digest: BTreeMap<&[u8], i64> = parent_payloads
        .iter()
        .map(|(ordinal, digest)| (digest.as_slice(), *ordinal))
        .collect();
    let occurrences: Vec<(i64, i64)> = {
        let mut statement = transaction
            .prepare("SELECT ordinal, payload_ordinal FROM clone_occurrences ORDER BY ordinal")
            .map_err(sqlite_error)?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?
    };
    let mut numbered = BTreeMap::<Vec<u8>, i64>::new();
    let mut fresh = Vec::<(i64, [u8; 32], &[u8])>::new();
    let mut repointed = Vec::new();
    for (ordinal, payload_ordinal) in occurrences {
        checkpoint(control)?;
        let digest: Vec<u8> = match successor_payloads.get(&ordinal) {
            Some((digest, _)) => digest.to_vec(),
            None => parent_payloads
                .get(&payload_ordinal)
                .cloned()
                .ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "carried clone occurrence names a missing payload".to_owned(),
                    )
                })?,
        };
        let next = numbered.len() as i64 + 1;
        let number = *numbered.entry(digest.clone()).or_insert_with(|| {
            if !parent_by_digest.contains_key(digest.as_slice())
                && let Some((key, payload)) = successor_payloads.get(&ordinal)
            {
                fresh.push((next, *key, payload));
            }
            next
        });
        if number != payload_ordinal {
            repointed.push((ordinal, number));
        }
    }
    // A successor body whose payload the parent already stores must match it.
    for (key, payload) in successor_payloads.values() {
        checkpoint(control)?;
        if let Some(ordinal) = parent_by_digest.get(key.as_slice()) {
            let stored: Vec<u8> = transaction
                .query_row(
                    "SELECT payload FROM clone_body_payloads WHERE ordinal = ?1",
                    [ordinal],
                    |row| row.get(0),
                )
                .map_err(sqlite_error)?;
            if stored.as_slice() != *payload {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "clone payload digest names different bytes".to_owned(),
                ));
            }
        }
    }
    for (ordinal, digest) in parent_payloads {
        checkpoint(control)?;
        match numbered.get(digest) {
            None => {
                transaction
                    .execute(
                        "DELETE FROM clone_body_payloads WHERE ordinal = ?1",
                        [ordinal],
                    )
                    .map_err(sqlite_error)?;
            }
            Some(number) if number != ordinal => {
                transaction
                    .execute(
                        "UPDATE clone_body_payloads SET ordinal = ?2 WHERE ordinal = ?1",
                        params![ordinal, -number - 1],
                    )
                    .map_err(sqlite_error)?;
            }
            Some(_) => {}
        }
    }
    restore_clone_payload_ordinals(transaction, control)?;
    {
        let mut insert = transaction
            .prepare_cached(
                "INSERT INTO clone_body_payloads(ordinal, payload_digest, payload) VALUES (?1, ?2, ?3)",
            )
            .map_err(sqlite_error)?;
        for (number, key, payload) in fresh {
            checkpoint(control)?;
            insert
                .execute(params![number, key.as_slice(), payload])
                .map_err(sqlite_error)?;
        }
    }
    let mut repoint = transaction
        .prepare_cached("UPDATE clone_occurrences SET payload_ordinal = ?2 WHERE ordinal = ?1")
        .map_err(sqlite_error)?;
    for (ordinal, number) in repointed {
        checkpoint(control)?;
        repoint
            .execute(params![ordinal, number])
            .map_err(sqlite_error)?;
    }
    // The final SQLite write can consume the deadline or observe cancellation.
    // Do not report a completed carry without checking that boundary as well.
    checkpoint(control)
}

#[cfg(test)]
mod payload_cancellation;
#[cfg(test)]
mod tests;
