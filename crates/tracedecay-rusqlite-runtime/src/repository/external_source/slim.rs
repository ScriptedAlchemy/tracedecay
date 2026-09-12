//! Slim receipt rows.
//!
//! A commit or projection receipt embeds its mutations and its aggregate
//! frontiers, and every one of those payloads already has a home of its own:
//! mutations in `external_source_mutations_v1`, effects in
//! `external_source_projection_effects_v2`, and — as of this shape — frontiers
//! in `external_source_frontiers_v1`. Persisted receipts therefore carry
//! digests where the domain type carries payloads (2 KB of mutation and 500 B
//! of frontier per message became 5 KB of receipt on one store, 2.5 GB in all)
//! and are hydrated back into the exact domain type on read. Hydration goes
//! through `serde_json::Value` so the domain types keep their private fields
//! and their own digest validation decides whether the rebuilt receipt is the
//! one that was committed.

use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use tracedecay_store::{SourceCommitReceiptV1, SourceProjectionCommitV1};

use super::super::support::invalid;

const MUTATIONS: &str = "mutations";
const EFFECTS: &str = "effects";
const RECEIPT_DIGEST: &str = "receipt_digest";
const MUTATION_DIGEST: &str = "mutation_digest";
const FRONTIER_DIGEST: &str = "digest";
const COMMIT_FRONTIERS: [&str; 2] = ["prior_source_frontier", "source_frontier"];
const PROJECTION_FRONTIERS: [&str; 2] = ["expected_projection_frontier", "source_frontier"];

/// One receipt reduced to digests, plus the frontier payloads it referenced,
/// for the caller to store beside it.
pub(super) struct SlimReceiptV1 {
    pub(super) json: String,
    /// `(frontier_digest, frontier_json)` for every frontier the receipt named.
    pub(super) frontiers: Vec<(String, String)>,
}

pub(super) fn slim_commit_receipt(
    receipt: &SourceCommitReceiptV1,
) -> rusqlite::Result<SlimReceiptV1> {
    let mut value = serde_json::to_value(receipt).map_err(|error| invalid(error.to_string()))?;
    let frontiers = detach_frontiers(&mut value, &COMMIT_FRONTIERS)?;
    detach_mutations(&mut value)?;
    Ok(SlimReceiptV1 {
        json: serde_json::to_string(&value).map_err(|error| invalid(error.to_string()))?,
        frontiers,
    })
}

pub(super) fn slim_projection_receipt(
    projection: &SourceProjectionCommitV1,
) -> rusqlite::Result<SlimReceiptV1> {
    let mut value =
        serde_json::to_value(projection).map_err(|error| invalid(error.to_string()))?;
    let frontiers = detach_frontiers(&mut value, &PROJECTION_FRONTIERS)?;
    detach_mutations(&mut value)?;
    // Effects live in `external_source_projection_effects_v2`, ordered by
    // `effect_index`, and hydrate from there.
    object(&mut value)?.insert(EFFECTS.to_owned(), Value::Array(Vec::new()));
    Ok(SlimReceiptV1 {
        json: serde_json::to_string(&value).map_err(|error| invalid(error.to_string()))?,
        frontiers,
    })
}

pub(super) fn hydrate_commit_receipt(
    connection: &rusqlite::Connection,
    binding_id: &str,
    slim: &str,
) -> rusqlite::Result<SourceCommitReceiptV1> {
    let mut value: Value = serde_json::from_str(slim).map_err(|error| invalid(error.to_string()))?;
    attach_frontiers(connection, binding_id, &mut value, &COMMIT_FRONTIERS)?;
    attach_mutations(connection, binding_id, &mut value)?;
    serde_json::from_value(value).map_err(|error| invalid(error.to_string()))
}

pub(super) fn hydrate_projection_receipt(
    connection: &rusqlite::Connection,
    binding_id: &str,
    slim: &str,
) -> rusqlite::Result<SourceProjectionCommitV1> {
    let mut value: Value = serde_json::from_str(slim).map_err(|error| invalid(error.to_string()))?;
    attach_frontiers(connection, binding_id, &mut value, &PROJECTION_FRONTIERS)?;
    attach_mutations(connection, binding_id, &mut value)?;
    let projection_digest = object(&mut value)?
        .get(RECEIPT_DIGEST)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("external source projection receipt has no receipt digest"))?
        .to_owned();
    let mut statement = connection.prepare_cached(
        "SELECT effect_json FROM external_source_projection_effects_v2
         WHERE binding_id = ?1 AND projection_digest = ?2
         ORDER BY effect_index",
    )?;
    let effects = statement
        .query_map(params![binding_id, projection_digest], |row| {
            serde_json::from_str::<Value>(&row.get::<_, String>(0)?)
                .map_err(|error| invalid(error.to_string()))
        })?
        .collect::<rusqlite::Result<Vec<Value>>>()?;
    object(&mut value)?.insert(EFFECTS.to_owned(), Value::Array(effects));
    serde_json::from_value(value).map_err(|error| invalid(error.to_string()))
}

/// Two persisted encodings agree when they denote the same JSON document. A
/// row migrated in SQL is minified differently from one serde wrote, so a
/// byte comparison would report a collision on an idempotent replay of a
/// pre-migration commit.
pub(super) fn same_json(stored: &str, expected: &str) -> bool {
    if stored == expected {
        return true;
    }
    match (
        serde_json::from_str::<Value>(stored),
        serde_json::from_str::<Value>(expected),
    ) {
        (Ok(stored), Ok(expected)) => stored == expected,
        _ => false,
    }
}

fn object(value: &mut Value) -> rusqlite::Result<&mut serde_json::Map<String, Value>> {
    value
        .as_object_mut()
        .ok_or_else(|| invalid("external source receipt encoding is not a JSON object"))
}

fn detach_frontiers(
    value: &mut Value,
    fields: &[&str],
) -> rusqlite::Result<Vec<(String, String)>> {
    let mut frontiers = Vec::with_capacity(fields.len());
    let object = object(value)?;
    for field in fields {
        let Some(frontier) = object.get(*field) else {
            continue;
        };
        if frontier.is_null() {
            continue;
        }
        let digest = frontier
            .get(FRONTIER_DIGEST)
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("external source frontier has no digest"))?
            .to_owned();
        let encoded =
            serde_json::to_string(frontier).map_err(|error| invalid(error.to_string()))?;
        frontiers.push((digest.clone(), encoded));
        object.insert((*field).to_owned(), Value::String(digest));
    }
    Ok(frontiers)
}

fn detach_mutations(value: &mut Value) -> rusqlite::Result<()> {
    let object = object(value)?;
    let Some(Value::Array(mutations)) = object.get(MUTATIONS) else {
        return Err(invalid("external source receipt has no mutation list"));
    };
    let digests = mutations
        .iter()
        .map(|mutation| {
            mutation
                .get(MUTATION_DIGEST)
                .and_then(Value::as_str)
                .map(|digest| Value::String(digest.to_owned()))
                .ok_or_else(|| invalid("external source mutation has no digest"))
        })
        .collect::<rusqlite::Result<Vec<Value>>>()?;
    object.insert(MUTATIONS.to_owned(), Value::Array(digests));
    Ok(())
}

fn attach_frontiers(
    connection: &rusqlite::Connection,
    binding_id: &str,
    value: &mut Value,
    fields: &[&str],
) -> rusqlite::Result<()> {
    let object = object(value)?;
    for field in fields {
        let Some(Value::String(digest)) = object.get(*field) else {
            continue;
        };
        let frontier: Option<String> = connection
            .prepare_cached(
                "SELECT frontier_json FROM external_source_frontiers_v1
                 WHERE binding_id = ?1 AND frontier_digest = ?2",
            )?
            .query_row(params![binding_id, digest], |row| row.get(0))
            .optional()?;
        let frontier = frontier.ok_or_else(|| {
            invalid("external source receipt names a frontier absent from history")
        })?;
        let frontier: Value =
            serde_json::from_str(&frontier).map_err(|error| invalid(error.to_string()))?;
        object.insert((*field).to_owned(), frontier);
    }
    Ok(())
}

fn attach_mutations(
    connection: &rusqlite::Connection,
    binding_id: &str,
    value: &mut Value,
) -> rusqlite::Result<()> {
    let object = object(value)?;
    let Some(Value::Array(digests)) = object.get(MUTATIONS) else {
        return Err(invalid("external source receipt has no mutation list"));
    };
    let mut statement = connection.prepare_cached(
        "SELECT mutation_json FROM external_source_mutations_v1
         WHERE binding_id = ?1 AND mutation_digest = ?2",
    )?;
    let mut mutations = Vec::with_capacity(digests.len());
    for digest in digests {
        let Value::String(digest) = digest else {
            return Err(invalid("external source receipt mutation reference is not a digest"));
        };
        let encoded: Option<String> = statement
            .query_row(params![binding_id, digest], |row| row.get(0))
            .optional()?;
        let encoded = encoded.ok_or_else(|| {
            invalid("external source receipt names a mutation absent from history")
        })?;
        mutations.push(serde_json::from_str(&encoded).map_err(|error| invalid(error.to_string()))?);
    }
    object.insert(MUTATIONS.to_owned(), Value::Array(mutations));
    Ok(())
}
