//! Tests for MERGE operator rollback behavior.
//!
//! Verifies that ON MATCH SET properties written by MERGE are correctly
//! undone when a transaction is rolled back.

use grafeo_common::types::Value;
use grafeo_engine::GrafeoDB;

#[test]
fn test_merge_on_match_set_rollback() {
    let db = GrafeoDB::new_in_memory();
    let mut session = db.session();

    // Create existing node
    session
        .execute("INSERT (:Person {name: 'Alix', status: 'active'})")
        .unwrap();

    // MERGE should match the existing node and apply ON MATCH SET
    session.begin_transaction().unwrap();
    session
        .execute("MERGE (p:Person {name: 'Alix'}) ON MATCH SET p.status = 'inactive'")
        .unwrap();

    // Verify the property was updated within the transaction
    let result = session
        .execute("MATCH (p:Person {name: 'Alix'}) RETURN p.status")
        .unwrap();
    assert_eq!(result.rows()[0][0], Value::String("inactive".into()));

    session.rollback().unwrap();

    // Property should be restored to original value
    let result = session
        .execute("MATCH (p:Person {name: 'Alix'}) RETURN p.status")
        .unwrap();
    assert_eq!(
        result.rows()[0][0],
        Value::String("active".into()),
        "status should be restored to 'active' after rollback"
    );
}

#[test]
fn test_merge_on_match_set_new_property_rollback() {
    let db = GrafeoDB::new_in_memory();
    let mut session = db.session();

    // Create existing node without 'updated' property
    session.execute("INSERT (:Person {name: 'Gus'})").unwrap();

    // MERGE adds a new property via ON MATCH SET
    session.begin_transaction().unwrap();
    session
        .execute("MERGE (p:Person {name: 'Gus'}) ON MATCH SET p.updated = true")
        .unwrap();
    session.rollback().unwrap();

    // New property should not exist after rollback
    let result = session
        .execute("MATCH (p:Person {name: 'Gus'}) RETURN p.updated")
        .unwrap();
    assert_eq!(
        result.rows()[0][0],
        Value::Null,
        "'updated' property should not exist after rollback"
    );
}

#[test]
fn test_merge_create_rolled_back_does_not_orphan_node() {
    // A MERGE that creates a new node, then has its surrounding
    // transaction rolled back, must not leave the new node visible. The
    // operator used to call `create_node_with_props`, which in turn
    // tagged the create with `TransactionId::SYSTEM` rather than the
    // active session transaction; rollback then could not undo it via
    // the MVCC undo log and the node persisted as an orphan.
    let db = GrafeoDB::new_in_memory();
    let mut session = db.session();

    session.begin_transaction().unwrap();
    session.execute("MERGE (n:OrphanCheck {val: 1})").unwrap();
    session.rollback().unwrap();

    let result = session
        .execute("MATCH (n:OrphanCheck) RETURN count(n) AS cnt")
        .unwrap();
    assert_eq!(
        result.rows()[0][0],
        Value::Int64(0),
        "MERGE-created node must be undone by rollback (no orphan)"
    );
}

#[test]
fn test_merge_relationship_create_rolled_back_does_not_orphan_edge() {
    // Same concern as the node case for the relationship MERGE path:
    // `create_edge_with_props` tagged the create with SYSTEM and the
    // edge survived rollback.
    let db = GrafeoDB::new_in_memory();
    let mut session = db.session();

    session.execute("INSERT (:N {name: 'Alix'})").unwrap();
    session.execute("INSERT (:N {name: 'Gus'})").unwrap();

    session.begin_transaction().unwrap();
    session
        .execute(
            "MATCH (a:N {name: 'Alix'}), (b:N {name: 'Gus'}) \
             MERGE (a)-[:KNOWS]->(b)",
        )
        .unwrap();
    session.rollback().unwrap();

    let result = session
        .execute("MATCH ()-[k:KNOWS]->() RETURN count(k) AS cnt")
        .unwrap();
    assert_eq!(
        result.rows()[0][0],
        Value::Int64(0),
        "MERGE-created edge must be undone by rollback (no orphan)"
    );
}

#[test]
fn test_merge_two_phase_on_create_failure_rolls_back_node() {
    // When a UNIQUE constraint conflict on an ON CREATE expression
    // property triggers a phase-two validation failure, the partial
    // node created in phase one must not survive: the operator's error
    // propagates to the auto-commit wrapper, which rolls the
    // transaction back. With the fix the MVCC undo discards the
    // create; previously the SYSTEM-tagged create persisted.
    let db = GrafeoDB::new_in_memory();
    let session = db.session();

    // Schema that requires `slug` to be unique on :Article.
    session
        .execute("CREATE NODE TYPE Article (val INT64, slug STRING)")
        .unwrap();
    session
        .execute_cypher("CREATE CONSTRAINT unique_slug FOR (a:Article) REQUIRE a.slug IS UNIQUE")
        .unwrap();
    session
        .execute("INSERT (:Article {val: 0, slug: 'taken'})")
        .unwrap();

    // ON CREATE with a self-referencing expression forces the two-phase
    // path. The expression collapses to the constant 'taken', which
    // collides with the UNIQUE constraint and must fail in phase two.
    let err = session
        .execute(
            "MERGE (a:Article {val: 1}) \
             ON CREATE SET a.slug = coalesce(a.slug, 'taken')",
        )
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("UNIQUE") || msg.contains("unique") || msg.contains("constraint"),
        "expected uniqueness violation error, got: {msg}"
    );

    // The phase-one node must have been rolled back: only the
    // pre-existing :Article (val=0) should remain.
    let result = session
        .execute("MATCH (a:Article) RETURN count(a) AS cnt")
        .unwrap();
    assert_eq!(
        result.rows()[0][0],
        Value::Int64(1),
        "phase-one node must not survive a phase-two ON CREATE failure"
    );
}

#[test]
fn test_merge_on_match_committed_stays() {
    let db = GrafeoDB::new_in_memory();
    let mut session = db.session();

    session
        .execute("INSERT (:Person {name: 'Vincent', score: 0})")
        .unwrap();

    // MERGE with commit: property should persist
    session.begin_transaction().unwrap();
    session
        .execute("MERGE (p:Person {name: 'Vincent'}) ON MATCH SET p.score = 100")
        .unwrap();
    session.commit().unwrap();

    let result = session
        .execute("MATCH (p:Person {name: 'Vincent'}) RETURN p.score")
        .unwrap();
    assert_eq!(
        result.rows()[0][0],
        Value::Int64(100),
        "score should retain the committed value"
    );
}
