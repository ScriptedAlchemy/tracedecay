//! Slim receipt rows.
//!
//! A commit or projection receipt embeds its mutations and its aggregate
//! frontiers, and every one of those payloads already has a home of its own:
//! mutations in `external_source_mutations_v1`, effects in
//! `external_source_projection_effects_v2`, and, as of this shape, frontiers
//! in `external_source_frontiers_v1`. Persisted receipts therefore carry
//! digests where the domain type carries payloads (2 KB of mutation and 500 B
//! of frontier per message became 5 KB of receipt on one store, 2.5 GB in all)
//! and are hydrated back into the exact domain type on read. Hydration goes
//! through `serde_json::Value` so the domain types keep their private fields
//! and their own digest validation decides whether the rebuilt receipt is the
//! one that was committed.
//!
//! A history row's mutation likewise drops every value its row and binding
//! already carry: the binding identity, the native-object, revision, and
//! mutation digests, and the evidence fields that repeat the observation. A
//! value is dropped only when it equals what hydration restores, so a
//! mutation that disagrees with its row keeps its own value and fails its
//! domain validation instead of silently adopting the row's.

use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value};
use tracedecay_domain::SourceBindingIdentityV1;
use tracedecay_store::{SourceCommitReceiptV1, SourceObjectMutationV1, SourceProjectionCommitV1};

use super::super::support::invalid;

const MUTATIONS: &str = "mutations";
const EFFECTS: &str = "effects";
const RECEIPT_DIGEST: &str = "receipt_digest";
const MUTATION_DIGEST: &str = "mutation_digest";
const FRONTIER_DIGEST: &str = "digest";
const COMMIT_FRONTIERS: [&str; 2] = ["prior_source_frontier", "source_frontier"];
const PROJECTION_FRONTIERS: [&str; 2] = ["expected_projection_frontier", "source_frontier"];
const OBSERVATION: &str = "observation";
const EVIDENCE: &str = "evidence";
const BINDING: &str = "binding";
const NATIVE_OBJECT: &str = "native_object";
const REVISION: &str = "revision";
/// Evidence fields that restate the mutation's observation.
const EVIDENCE_OBSERVATION_FIELDS: [&str; 3] = [NATIVE_OBJECT, REVISION, "sanitized_digest"];

/// The digests a mutation history row stores as columns.
pub(super) struct MutationRowKeys<'a> {
    pub(super) native_object: &'a str,
    pub(super) revision: &'a str,
    pub(super) mutation_digest: &'a str,
}

pub(super) fn slim_mutation(
    mutation: &SourceObjectMutationV1,
    binding: &SourceBindingIdentityV1,
) -> rusqlite::Result<String> {
    let mut value = serde_json::to_value(mutation).map_err(|error| invalid(error.to_string()))?;
    let binding = serde_json::to_value(binding).map_err(|error| invalid(error.to_string()))?;
    let root = object(&mut value)?;
    let observation = root
        .get(OBSERVATION)
        .cloned()
        .ok_or_else(|| invalid("external source mutation has no observation"))?;
    let evidence = nested(root, EVIDENCE)?;
    drop_if_equal(evidence, BINDING, &binding);
    for field in EVIDENCE_OBSERVATION_FIELDS {
        if let Some(expected) = observation.get(field) {
            drop_if_equal(evidence, field, expected);
        }
    }
    let observation = nested(root, OBSERVATION)?;
    drop_if_equal(
        observation,
        NATIVE_OBJECT,
        &Value::String(
            mutation
                .observation()
                .native_object()
                .digest()
                .as_str()
                .to_owned(),
        ),
    );
    drop_if_equal(
        observation,
        REVISION,
        &Value::String(
            mutation
                .observation()
                .revision()
                .digest()
                .as_str()
                .to_owned(),
        ),
    );
    drop_if_equal(
        root,
        MUTATION_DIGEST,
        &Value::String(mutation.mutation_digest().as_str().to_owned()),
    );
    serde_json::to_string(&value).map_err(|error| invalid(error.to_string()))
}

pub(super) fn hydrate_mutation(
    slim: &str,
    binding: &SourceBindingIdentityV1,
    keys: MutationRowKeys<'_>,
) -> rusqlite::Result<SourceObjectMutationV1> {
    serde_json::from_value(hydrate_mutation_value(slim, binding, keys)?)
        .map_err(|error| invalid(error.to_string()))
}

fn hydrate_mutation_value(
    slim: &str,
    binding: &SourceBindingIdentityV1,
    keys: MutationRowKeys<'_>,
) -> rusqlite::Result<Value> {
    let mut value: Value =
        serde_json::from_str(slim).map_err(|error| invalid(error.to_string()))?;
    let binding = serde_json::to_value(binding).map_err(|error| invalid(error.to_string()))?;
    let root = object(&mut value)?;
    restore(
        root,
        MUTATION_DIGEST,
        Value::String(keys.mutation_digest.to_owned()),
    );
    let observation = nested(root, OBSERVATION)?;
    restore(
        observation,
        NATIVE_OBJECT,
        Value::String(keys.native_object.to_owned()),
    );
    restore(
        observation,
        REVISION,
        Value::String(keys.revision.to_owned()),
    );
    let observation = observation.clone();
    let evidence = nested(root, EVIDENCE)?;
    restore(evidence, BINDING, binding);
    for field in EVIDENCE_OBSERVATION_FIELDS {
        if let Some(value) = observation.get(field) {
            restore(evidence, field, value.clone());
        }
    }
    Ok(value)
}

fn nested<'a>(
    root: &'a mut Map<String, Value>,
    field: &str,
) -> rusqlite::Result<&'a mut Map<String, Value>> {
    root.get_mut(field)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invalid("external source mutation encoding is missing an object field"))
}

fn drop_if_equal(object: &mut Map<String, Value>, field: &str, expected: &Value) {
    if object.get(field) == Some(expected) {
        object.remove(field);
    }
}

fn restore(object: &mut Map<String, Value>, field: &str, value: Value) {
    object.entry(field).or_insert(value);
}

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
    let mut value = serde_json::to_value(projection).map_err(|error| invalid(error.to_string()))?;
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
    binding: &SourceBindingIdentityV1,
    slim: &str,
) -> rusqlite::Result<SourceCommitReceiptV1> {
    let binding_id = binding.binding_id.as_str();
    let mut value: Value =
        serde_json::from_str(slim).map_err(|error| invalid(error.to_string()))?;
    attach_frontiers(connection, binding_id, &mut value, &COMMIT_FRONTIERS)?;
    attach_mutations(connection, binding, &mut value)?;
    serde_json::from_value(value).map_err(|error| invalid(error.to_string()))
}

pub(super) fn hydrate_projection_receipt(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
    slim: &str,
) -> rusqlite::Result<SourceProjectionCommitV1> {
    let binding_id = binding.binding_id.as_str();
    let mut value: Value =
        serde_json::from_str(slim).map_err(|error| invalid(error.to_string()))?;
    attach_frontiers(connection, binding_id, &mut value, &PROJECTION_FRONTIERS)?;
    attach_mutations(connection, binding, &mut value)?;
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

fn object(value: &mut Value) -> rusqlite::Result<&mut serde_json::Map<String, Value>> {
    value
        .as_object_mut()
        .ok_or_else(|| invalid("external source receipt encoding is not a JSON object"))
}

fn detach_frontiers(value: &mut Value, fields: &[&str]) -> rusqlite::Result<Vec<(String, String)>> {
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
    binding: &SourceBindingIdentityV1,
    value: &mut Value,
) -> rusqlite::Result<()> {
    let object = object(value)?;
    let Some(Value::Array(digests)) = object.get(MUTATIONS) else {
        return Err(invalid("external source receipt has no mutation list"));
    };
    let mut statement = connection.prepare_cached(
        "SELECT mutation_json, native_object_digest, revision_digest
         FROM external_source_mutations_v1
         WHERE binding_id = ?1 AND mutation_digest = ?2",
    )?;
    let mut mutations = Vec::with_capacity(digests.len());
    for digest in digests {
        let Value::String(digest) = digest else {
            return Err(invalid(
                "external source receipt mutation reference is not a digest",
            ));
        };
        let row: Option<(String, String, String)> = statement
            .query_row(params![binding.binding_id.as_str(), digest], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?;
        let (slim, native_object, revision) = row.ok_or_else(|| {
            invalid("external source receipt names a mutation absent from history")
        })?;
        mutations.push(hydrate_mutation_value(
            &slim,
            binding,
            MutationRowKeys {
                native_object: &native_object,
                revision: &revision,
                mutation_digest: digest,
            },
        )?);
    }
    object.insert(MUTATIONS.to_owned(), Value::Array(mutations));
    Ok(())
}
