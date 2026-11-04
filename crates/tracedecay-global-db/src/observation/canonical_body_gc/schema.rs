use std::collections::BTreeMap;

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use super::{db_error, integrity_error};

/// Derived ownership, never another copy of a body. The bootstrap progress is
/// committed with installation of every writer trigger, before deletion is
/// permitted. Admission can install this component without rewriting evidence.
pub(crate) const OBJECTS: &[(&str, &str)] = &[
    (
        "canonical_body_gc_refs",
        "CREATE TABLE canonical_body_gc_refs (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('raw', 'observation')),
            owner_id INTEGER NOT NULL,
            content_hash TEXT NOT NULL,
            PRIMARY KEY(owner_kind, owner_id, content_hash),
            FOREIGN KEY(content_hash) REFERENCES session_canonical_bodies(content_hash)
                ON DELETE RESTRICT
        )",
    ),
    (
        "canonical_body_gc_by_hash",
        "CREATE INDEX canonical_body_gc_by_hash ON canonical_body_gc_refs(content_hash)",
    ),
    (
        "canonical_body_gc_candidates",
        "CREATE TABLE canonical_body_gc_candidates (
            content_hash TEXT NOT NULL PRIMARY KEY,
            FOREIGN KEY(content_hash) REFERENCES session_canonical_bodies(content_hash)
                ON DELETE CASCADE
        )",
    ),
    (
        "canonical_body_gc_progress",
        "CREATE TABLE canonical_body_gc_progress (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            raw_through INTEGER NOT NULL CHECK(raw_through >= 0),
            raw_until INTEGER NOT NULL CHECK(raw_until >= raw_through),
            observations_through INTEGER NOT NULL CHECK(observations_through >= 0),
            observations_until INTEGER NOT NULL CHECK(observations_until >= observations_through),
            bodies_after TEXT NOT NULL,
            bodies_until TEXT NOT NULL CHECK(bodies_until >= bodies_after)
        )",
    ),
    (
        "canonical_body_gc_body_insert",
        "CREATE TRIGGER canonical_body_gc_body_insert
         AFTER INSERT ON session_canonical_bodies BEGIN
            INSERT OR IGNORE INTO canonical_body_gc_candidates(content_hash)
            VALUES (NEW.content_hash);
         END",
    ),
    (
        "canonical_body_gc_ref_delete",
        "CREATE TRIGGER canonical_body_gc_ref_delete
         AFTER DELETE ON canonical_body_gc_refs BEGIN
            INSERT OR IGNORE INTO canonical_body_gc_candidates(content_hash)
            VALUES (OLD.content_hash);
         END",
    ),
    (
        "canonical_body_gc_raw_insert",
        "CREATE TRIGGER canonical_body_gc_raw_insert
         AFTER INSERT ON lcm_raw_messages
         WHEN NEW.storage_kind = 'inline' AND NEW.content IS NULL BEGIN
            INSERT OR IGNORE INTO canonical_body_gc_refs(owner_kind, owner_id, content_hash)
            VALUES ('raw', NEW.store_id, NEW.content_hash);
         END",
    ),
    (
        "canonical_body_gc_raw_delete",
        "CREATE TRIGGER canonical_body_gc_raw_delete
         AFTER DELETE ON lcm_raw_messages BEGIN
            DELETE FROM canonical_body_gc_refs
            WHERE owner_kind = 'raw' AND owner_id = OLD.store_id;
         END",
    ),
    (
        "canonical_body_gc_raw_update",
        "CREATE TRIGGER canonical_body_gc_raw_update
         AFTER UPDATE OF store_id, storage_kind, content, content_hash ON lcm_raw_messages
         WHEN OLD.store_id IS NOT NEW.store_id
           OR OLD.content_hash IS NOT NEW.content_hash
           OR (OLD.storage_kind = 'inline' AND OLD.content IS NULL)
                IS NOT (NEW.storage_kind = 'inline' AND NEW.content IS NULL)
         BEGIN
            DELETE FROM canonical_body_gc_refs
            WHERE owner_kind = 'raw' AND owner_id = OLD.store_id;
            INSERT OR IGNORE INTO canonical_body_gc_refs(owner_kind, owner_id, content_hash)
            SELECT 'raw', NEW.store_id, NEW.content_hash
            WHERE NEW.storage_kind = 'inline' AND NEW.content IS NULL;
         END",
    ),
    (
        "canonical_body_gc_observation_insert",
        "CREATE TRIGGER canonical_body_gc_observation_insert
         AFTER INSERT ON observations BEGIN
            SELECT CASE
                WHEN NOT json_valid(NEW.observation_json)
                    THEN RAISE(ABORT, 'invalid canonical body reference document')
                WHEN json_type(NEW.observation_json, '$._tracedecay_canonical_body_refs')
                    IS NOT NULL AND json_type(NEW.observation_json,
                        '$._tracedecay_canonical_body_refs') <> 'object'
                    THEN RAISE(ABORT, 'invalid canonical body reference map')
                WHEN EXISTS (SELECT 1 FROM json_each(NEW.observation_json,
                        '$._tracedecay_canonical_body_refs') WHERE type <> 'text')
                    THEN RAISE(ABORT, 'invalid canonical body reference hash')
            END;
            INSERT OR IGNORE INTO canonical_body_gc_refs(owner_kind, owner_id, content_hash)
            SELECT 'observation', NEW.sequence, value
            FROM json_each(NEW.observation_json, '$._tracedecay_canonical_body_refs');
         END",
    ),
    (
        "canonical_body_gc_observation_delete",
        "CREATE TRIGGER canonical_body_gc_observation_delete
         AFTER DELETE ON observations BEGIN
            DELETE FROM canonical_body_gc_refs
            WHERE owner_kind = 'observation' AND owner_id = OLD.sequence;
         END",
    ),
    (
        "canonical_body_gc_observation_update",
        "CREATE TRIGGER canonical_body_gc_observation_update
         AFTER UPDATE OF sequence, observation_json ON observations
         WHEN OLD.sequence IS NOT NEW.sequence OR OLD.observation_json IS NOT NEW.observation_json
         BEGIN
            SELECT CASE
                WHEN NOT json_valid(NEW.observation_json)
                    THEN RAISE(ABORT, 'invalid canonical body reference document')
                WHEN json_type(NEW.observation_json, '$._tracedecay_canonical_body_refs')
                    IS NOT NULL AND json_type(NEW.observation_json,
                        '$._tracedecay_canonical_body_refs') <> 'object'
                    THEN RAISE(ABORT, 'invalid canonical body reference map')
                WHEN EXISTS (SELECT 1 FROM json_each(NEW.observation_json,
                        '$._tracedecay_canonical_body_refs') WHERE type <> 'text')
                    THEN RAISE(ABORT, 'invalid canonical body reference hash')
            END;
            DELETE FROM canonical_body_gc_refs
            WHERE owner_kind = 'observation' AND owner_id = OLD.sequence;
            INSERT OR IGNORE INTO canonical_body_gc_refs(owner_kind, owner_id, content_hash)
            SELECT 'observation', NEW.sequence, value
            FROM json_each(NEW.observation_json, '$._tracedecay_canonical_body_refs');
         END",
    ),
];

async fn inventory(
    conn: &(impl QueryExecutor + ?Sized),
) -> tracedecay_domain::errors::Result<BTreeMap<String, String>> {
    let mut rows = conn
        .query(
            "SELECT name, sql FROM sqlite_master
             WHERE name GLOB 'canonical_body_gc_*' ORDER BY name LIMIT ?1",
            params![OBJECTS.len() as i64 + 1],
        )
        .await
        .map_err(db_error)?;
    let mut objects = BTreeMap::new();
    while let Some(row) = rows.next().await.map_err(db_error)? {
        objects.insert(
            row.get::<String>(0).map_err(db_error)?,
            row.get::<String>(1).map_err(db_error)?,
        );
    }
    Ok(objects)
}

fn require_inventory(objects: &BTreeMap<String, String>) -> tracedecay_domain::errors::Result<()> {
    if objects.len() != OBJECTS.len() {
        return Err(integrity_error(
            "canonical body ownership schema is incomplete",
        ));
    }
    for &(name, expected) in OBJECTS {
        // These objects are installed from these exact statements. Preserve
        // case inside JSON paths and owner literals: case-folding a changed
        // trigger could hide a reference-tracking failure.
        if objects
            .get(name)
            .is_none_or(|actual| actual.trim().trim_end_matches(';') != expected)
        {
            return Err(integrity_error(format!(
                "canonical body ownership object {name} does not match its contract"
            )));
        }
    }
    Ok(())
}

pub(super) async fn validate(
    conn: &(impl QueryExecutor + ?Sized),
) -> tracedecay_domain::errors::Result<()> {
    require_inventory(&inventory(conn).await?)
}

pub(crate) async fn install(
    conn: &(impl Executor + ?Sized),
) -> tracedecay_domain::errors::Result<()> {
    let objects = inventory(conn).await?;
    if !objects.is_empty() {
        require_inventory(&objects)?;
        super::read_progress(conn).await?;
        return Ok(());
    }
    // The keyset bootstrap begins at zero. INTEGER PRIMARY KEY alone does
    // not forbid hand-written nonpositive identities, so prove the frontier
    // cannot skip an old owner before installing its collection authority.
    let mut minima = conn
        .query(
            "SELECT (SELECT MIN(store_id) FROM lcm_raw_messages),
                    (SELECT MIN(sequence) FROM observations)",
            (),
        )
        .await
        .map_err(db_error)?;
    let minima = minima
        .next()
        .await
        .map_err(db_error)?
        .ok_or_else(|| integrity_error("canonical body ownership minima are missing"))?;
    for index in 0..2 {
        if minima
            .get::<Option<i64>>(index)
            .map_err(db_error)?
            .is_some_and(|id| id <= 0)
        {
            return Err(integrity_error(
                "canonical body ownership requires positive source identities",
            ));
        }
    }
    for &(_, sql) in OBJECTS {
        conn.execute_batch(sql).await.map_err(db_error)?;
    }
    conn.execute(
        "INSERT INTO canonical_body_gc_progress (
            singleton, raw_through, raw_until, observations_through, observations_until,
            bodies_after, bodies_until
         ) SELECT 1, 0, COALESCE((SELECT MAX(store_id) FROM lcm_raw_messages), 0),
                   0, COALESCE((SELECT MAX(sequence) FROM observations), 0),
                   '', COALESCE((SELECT MAX(content_hash) FROM session_canonical_bodies), '')",
        (),
    )
    .await
    .map_err(db_error)?;
    Ok(())
}
