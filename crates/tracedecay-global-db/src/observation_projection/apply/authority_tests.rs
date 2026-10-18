use rusqlite::{Connection, Params};

use super::{
    LIVE_PROJECTION_AUTHORITY_SQL, REBUILD_PROJECTION_AUTHORITY_SQL,
    SESSION_MESSAGE_PROJECTOR_VERSION,
};

const AUTHORITY_TABLES: &str = "
CREATE TABLE observation_projection_dispositions (
    projector_version TEXT, observation_id TEXT, receipt_id TEXT, reason TEXT,
    PRIMARY KEY(projector_version, observation_id));
CREATE TABLE observation_projection_aliases (
    projector_version TEXT, observation_id TEXT, output_provider TEXT, output_message_id TEXT,
    PRIMARY KEY(projector_version, observation_id));
CREATE TABLE observation_projection_rebuild_aliases (
    projector_version TEXT, generation TEXT, observation_id TEXT,
    output_provider TEXT, output_message_id TEXT,
    PRIMARY KEY(projector_version, generation, observation_id));";

fn query_plan<P: Params>(connection: &Connection, sql: &str, params: P) -> Vec<String> {
    connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(params, |row| row.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn assert_point_read(plan: &[String], indexes: &[&str]) {
    for index in indexes {
        assert!(
            plan.iter().any(|detail| detail.contains(index)),
            "query plan did not use {index}: {plan:?}"
        );
    }
    assert!(
        plan.iter()
            .all(|detail| !detail.contains("SCAN observation_projection")),
        "query plan regressed to a table scan: {plan:?}"
    );
}

#[test]
fn projection_authority_is_one_indexed_statement() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(AUTHORITY_TABLES).unwrap();
    let live = query_plan(
        &connection,
        LIVE_PROJECTION_AUTHORITY_SQL,
        rusqlite::params![SESSION_MESSAGE_PROJECTOR_VERSION, "observation"],
    );
    assert_point_read(
        &live,
        &[
            "observation_projection_dispositions_1",
            "observation_projection_aliases_1",
        ],
    );
    let rebuild = query_plan(
        &connection,
        REBUILD_PROJECTION_AUTHORITY_SQL,
        rusqlite::params![
            SESSION_MESSAGE_PROJECTOR_VERSION,
            "generation",
            "observation"
        ],
    );
    assert_point_read(
        &rebuild,
        &[
            "observation_projection_dispositions_1",
            "observation_projection_rebuild_aliases_1",
        ],
    );
}
