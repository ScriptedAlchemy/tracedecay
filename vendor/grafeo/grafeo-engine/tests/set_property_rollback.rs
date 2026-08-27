//! Tests for SET property rollback behavior.
//!
//! Verifies that property mutations (SET, REMOVE) are correctly undone
//! when a transaction is rolled back.

use grafeo_common::types::Value;
use grafeo_engine::GrafeoDB;

// ============================================================================
// SET property rollback tests
// ============================================================================

#[test]
fn test_set_property_rollback_restores_original_value() {
    let db = GrafeoDB::new_in_memory();
    let mut session = db.session();

    // Create a node with a known property value
    session
        .execute("INSERT (:Account {owner: 'Alix', balance: 1000})")
        .unwrap();

    // Verify initial state
    let result = session
        .execute("MATCH (a:Account {owner: 'Alix'}) RETURN a.balance")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0][0], Value::Int64(1000));

    // Begin transaction, modify property, then rollback
    session.begin_transaction().unwrap();
    session
        .execute("MATCH (a:Account {owner: 'Alix'}) SET a.balance = 9999")
        .unwrap();
    session.rollback().unwrap();

    // Property should be restored to original value
    let result = session
        .execute("MATCH (a:Account {owner: 'Alix'}) RETURN a.balance")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(
        result.rows()[0][0],
        Value::Int64(1000),
        "balance should be restored to 1000 after rollback"
    );
}

#[test]
fn test_set_new_property_rollback_removes_it() {
    let db = GrafeoDB::new_in_memory();
    let mut session = db.session();

    // Create a node without the 'status' property
    session.execute("INSERT (:Account {owner: 'Gus'})").unwrap();

    // Verify no status property
    let result = session
        .execute("MATCH (a:Account {owner: 'Gus'}) RETURN a.status")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0][0], Value::Null);

    // Begin transaction, add a new property, then rollback
    session.begin_transaction().unwrap();
    session
        .execute("MATCH (a:Account {owner: 'Gus'}) SET a.status = 'active'")
        .unwrap();

    // Verify property exists within the transaction
    let result = session
        .execute("MATCH (a:Account {owner: 'Gus'}) RETURN a.status")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0][0], Value::String("active".into()));

    session.rollback().unwrap();

    // Property should be gone after rollback
    let result = session
        .execute("MATCH (a:Account {owner: 'Gus'}) RETURN a.status")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(
        result.rows()[0][0],
        Value::Null,
        "status property should not exist after rollback"
    );
}

#[test]
fn test_set_property_twice_rollback_restores_original() {
    let db = GrafeoDB::new_in_memory();
    let mut session = db.session();

    // Create a node with initial value
    session
        .execute("INSERT (:Account {owner: 'Vincent', balance: 500})")
        .unwrap();

    // Begin transaction, modify property twice, then rollback
    session.begin_transaction().unwrap();
    session
        .execute("MATCH (a:Account {owner: 'Vincent'}) SET a.balance = 1000")
        .unwrap();
    session
        .execute("MATCH (a:Account {owner: 'Vincent'}) SET a.balance = 2000")
        .unwrap();
    session.rollback().unwrap();

    // Should be restored to the original value (500), not to any intermediate value
    let result = session
        .execute("MATCH (a:Account {owner: 'Vincent'}) RETURN a.balance")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(
        result.rows()[0][0],
        Value::Int64(500),
        "balance should be restored to original 500 after rolling back two SETs"
    );
}

#[test]
fn test_insert_and_set_in_same_transaction_rollback() {
    let db = GrafeoDB::new_in_memory();
    let mut session = db.session();

    // Begin transaction, insert node, then modify it, then rollback
    session.begin_transaction().unwrap();
    session
        .execute("INSERT (:Temporary {name: 'Jules', value: 1})")
        .unwrap();
    session
        .execute("MATCH (t:Temporary {name: 'Jules'}) SET t.value = 42")
        .unwrap();
    session.rollback().unwrap();

    // Both the node and property change should be gone
    let result = session
        .execute("MATCH (t:Temporary {name: 'Jules'}) RETURN t.value")
        .unwrap();
    assert_eq!(
        result.row_count(),
        0,
        "node and property should not exist after rollback"
    );
}

#[test]
fn test_set_property_committed_stays() {
    let db = GrafeoDB::new_in_memory();
    let mut session = db.session();

    // Create a node with initial value
    session
        .execute("INSERT (:Account {owner: 'Mia', balance: 100})")
        .unwrap();

    // Begin transaction, modify property, then commit
    session.begin_transaction().unwrap();
    session
        .execute("MATCH (a:Account {owner: 'Mia'}) SET a.balance = 5000")
        .unwrap();
    session.commit().unwrap();

    // Property should have the committed value
    let result = session
        .execute("MATCH (a:Account {owner: 'Mia'}) RETURN a.balance")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(
        result.rows()[0][0],
        Value::Int64(5000),
        "balance should retain the committed value"
    );
}

#[test]
fn test_set_multiple_properties_rollback() {
    let db = GrafeoDB::new_in_memory();
    let mut session = db.session();

    // Create a node with multiple properties
    session
        .execute("INSERT (:Person {name: 'Butch', age: 35, city: 'Amsterdam'})")
        .unwrap();

    // Begin transaction, modify multiple properties, then rollback
    session.begin_transaction().unwrap();
    session
        .execute("MATCH (p:Person {name: 'Butch'}) SET p.age = 99, p.city = 'Berlin'")
        .unwrap();
    session.rollback().unwrap();

    // Both properties should be restored
    let result = session
        .execute("MATCH (p:Person {name: 'Butch'}) RETURN p.age, p.city")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(
        result.rows()[0][0],
        Value::Int64(35),
        "age should be restored to 35"
    );
    assert_eq!(
        result.rows()[0][1],
        Value::String("Amsterdam".into()),
        "city should be restored to Amsterdam"
    );
}

#[test]
fn test_autocommit_set_not_affected() {
    // Verify that auto-commit SET (outside a transaction) still works normally
    let db = GrafeoDB::new_in_memory();
    let session = db.session();

    session
        .execute("INSERT (:Config {key: 'version', value: 1})")
        .unwrap();
    session
        .execute("MATCH (c:Config {key: 'version'}) SET c.value = 2")
        .unwrap();

    let result = session
        .execute("MATCH (c:Config {key: 'version'}) RETURN c.value")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(
        result.rows()[0][0],
        Value::Int64(2),
        "auto-commit SET should persist"
    );
}

#[test]
fn test_set_then_commit_then_rollback_different_tx() {
    let db = GrafeoDB::new_in_memory();
    let mut session = db.session();

    // Create initial data
    session
        .execute("INSERT (:Counter {name: 'hits', count: 0})")
        .unwrap();

    // First transaction: set and commit
    session.begin_transaction().unwrap();
    session
        .execute("MATCH (c:Counter {name: 'hits'}) SET c.count = 10")
        .unwrap();
    session.commit().unwrap();

    // Second transaction: set and rollback
    session.begin_transaction().unwrap();
    session
        .execute("MATCH (c:Counter {name: 'hits'}) SET c.count = 999")
        .unwrap();
    session.rollback().unwrap();

    // Value should be from the first committed transaction (10)
    let result = session
        .execute("MATCH (c:Counter {name: 'hits'}) RETURN c.count")
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(
        result.rows()[0][0],
        Value::Int64(10),
        "count should be 10 (from committed tx), not 999 (from rolled back tx)"
    );
}
