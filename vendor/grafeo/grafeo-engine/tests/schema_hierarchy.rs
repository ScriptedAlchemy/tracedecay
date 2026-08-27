//! Integration tests for GQL Catalog/Schema/Graph hierarchy (ISO/IEC 39075).
//!
//! Verifies that schemas provide isolated namespaces with auto-created
//! default graphs, independent session state, and correct visibility rules.

use grafeo_engine::GrafeoDB;

fn db() -> GrafeoDB {
    GrafeoDB::new_in_memory()
}

// ── Default graph auto-creation ─────────────────────────────────

#[test]
fn create_schema_enables_default_graph() {
    let db = db();
    let s = db.session();

    s.execute("CREATE SCHEMA reporting").unwrap();
    s.execute("SESSION SET SCHEMA reporting").unwrap();

    // Should be able to insert without explicitly creating a graph
    s.execute("INSERT (:Report {title: 'Q1'})").unwrap();
    let result = s.execute("MATCH (n:Report) RETURN n.title").unwrap();
    assert_eq!(result.row_count(), 1);
}

// ── Schema default graph isolation ──────────────────────────────

#[test]
fn schema_default_isolated_from_global() {
    let db = db();
    let s = db.session();

    // Insert into global default
    s.execute("INSERT (:Global {v: 1})").unwrap();

    // Create schema and insert into its default
    s.execute("CREATE SCHEMA s1").unwrap();
    s.execute("SESSION SET SCHEMA s1").unwrap();
    s.execute("INSERT (:Local {v: 2})").unwrap();

    // Schema default should only see Local
    let result = s.execute("MATCH (n) RETURN n").unwrap();
    assert_eq!(result.row_count(), 1, "schema should only see its own data");

    // Reset back to global: should only see Global
    s.execute("SESSION RESET SCHEMA").unwrap();
    let result = s.execute("MATCH (n) RETURN n").unwrap();
    assert_eq!(
        result.row_count(),
        1,
        "global default should only see its own data"
    );
}

// ── Named graph within schema ───────────────────────────────────

#[test]
fn named_graph_within_schema() {
    let db = db();
    let s = db.session();

    s.execute("CREATE SCHEMA reports").unwrap();
    s.execute("SESSION SET SCHEMA reports").unwrap();
    s.execute("CREATE GRAPH quarterly").unwrap();
    s.execute("SESSION SET GRAPH quarterly").unwrap();
    s.execute("INSERT (:Report {q: 1})").unwrap();

    // Switch back to schema's default graph: should not see the data
    s.execute("SESSION RESET GRAPH").unwrap();
    let result = s.execute("MATCH (n:Report) RETURN n").unwrap();
    assert_eq!(
        result.row_count(),
        0,
        "schema default should not see data from named graph"
    );

    // Switch back to named graph: data is still there
    s.execute("SESSION SET GRAPH quarterly").unwrap();
    let result = s.execute("MATCH (n:Report) RETURN n").unwrap();
    assert_eq!(result.row_count(), 1);
}

// ── SHOW GRAPHS ─────────────────────────────────────────────────

#[test]
fn show_graphs_filters_and_hides_default() {
    let db = db();
    let s = db.session();

    s.execute("CREATE SCHEMA s1").unwrap();
    s.execute("SESSION SET SCHEMA s1").unwrap();
    s.execute("CREATE GRAPH alpha").unwrap();
    s.execute("CREATE GRAPH beta").unwrap();

    // SHOW GRAPHS should list alpha and beta, not __default__
    let result = s.execute("SHOW GRAPHS").unwrap();
    assert_eq!(result.row_count(), 2, "__default__ should be hidden");

    // Global scope sees no graphs (alpha/beta are in s1)
    s.execute("SESSION RESET SCHEMA").unwrap();
    let result = s.execute("SHOW GRAPHS").unwrap();
    assert_eq!(
        result.row_count(),
        0,
        "global scope should not see schema-scoped graphs"
    );
}

// ── Cross-schema isolation ──────────────────────────────────────

#[test]
fn cross_schema_data_isolation() {
    let db = db();
    let s = db.session();

    s.execute("CREATE SCHEMA a").unwrap();
    s.execute("CREATE SCHEMA b").unwrap();

    // Insert into schema a's default
    s.execute("SESSION SET SCHEMA a").unwrap();
    s.execute("INSERT (:Item {from: 'a'})").unwrap();

    // Switch to schema b: should see nothing
    s.execute("SESSION SET SCHEMA b").unwrap();
    let result = s.execute("MATCH (n) RETURN n").unwrap();
    assert_eq!(
        result.row_count(),
        0,
        "schema b should not see schema a data"
    );

    // Switch back to a: data still there
    s.execute("SESSION SET SCHEMA a").unwrap();
    let result = s.execute("MATCH (n) RETURN n").unwrap();
    assert_eq!(result.row_count(), 1);
}

// ── DROP SCHEMA ─────────────────────────────────────────────────

#[test]
fn drop_schema_blocks_when_user_graphs_exist() {
    let db = db();
    let s = db.session();

    s.execute("CREATE SCHEMA s1").unwrap();
    s.execute("SESSION SET SCHEMA s1").unwrap();
    s.execute("CREATE GRAPH myg").unwrap();
    s.execute("SESSION RESET SCHEMA").unwrap();

    let result = s.execute("DROP SCHEMA s1");
    assert!(
        result.is_err(),
        "should fail: schema has user-created graphs"
    );
}

#[test]
fn drop_schema_succeeds_with_only_default_graph() {
    let db = db();
    let s = db.session();

    s.execute("CREATE SCHEMA s1").unwrap();
    // Only __default__ exists, which should be auto-dropped
    s.execute("DROP SCHEMA s1").unwrap();

    let result = s.execute("SHOW SCHEMAS").unwrap();
    assert_eq!(result.row_count(), 0, "schema should be gone");
}

#[test]
fn drop_schema_resets_session_if_active() {
    let db = db();
    let s = db.session();

    s.execute("CREATE SCHEMA temp").unwrap();
    s.execute("SESSION SET SCHEMA temp").unwrap();
    s.execute("DROP SCHEMA temp").unwrap();

    // Session schema should have been reset
    assert!(
        s.current_schema().is_none(),
        "dropping active schema should reset session"
    );
}

// ── Session independence ────────────────────────────────────────

#[test]
fn setting_schema_does_not_reset_graph() {
    let db = db();
    let s = db.session();

    s.execute("CREATE GRAPH standalone").unwrap();
    s.execute("SESSION SET GRAPH standalone").unwrap();
    s.execute("CREATE SCHEMA s1").unwrap();
    s.execute("SESSION SET SCHEMA s1").unwrap();

    // Graph field is still "standalone"
    assert_eq!(s.current_graph(), Some("standalone".to_string()));
}

#[test]
fn setting_graph_does_not_reset_schema() {
    let db = db();
    let s = db.session();

    s.execute("CREATE SCHEMA s1").unwrap();
    s.execute("SESSION SET SCHEMA s1").unwrap();
    s.execute("CREATE GRAPH myg").unwrap();
    s.execute("SESSION SET GRAPH myg").unwrap();

    // Schema field is still "s1"
    assert_eq!(s.current_schema(), Some("s1".to_string()));
}

#[test]
fn reset_schema_independent_of_graph() {
    let db = db();
    let s = db.session();

    s.execute("CREATE SCHEMA s1").unwrap();
    s.execute("SESSION SET SCHEMA s1").unwrap();
    s.execute("CREATE GRAPH myg").unwrap();
    s.execute("SESSION SET GRAPH myg").unwrap();

    s.execute("SESSION RESET SCHEMA").unwrap();
    assert!(s.current_schema().is_none());
    assert_eq!(s.current_graph(), Some("myg".to_string()));
}

#[test]
fn reset_graph_independent_of_schema() {
    let db = db();
    let s = db.session();

    s.execute("CREATE SCHEMA s1").unwrap();
    s.execute("SESSION SET SCHEMA s1").unwrap();
    s.execute("CREATE GRAPH myg").unwrap();
    s.execute("SESSION SET GRAPH myg").unwrap();

    s.execute("SESSION RESET GRAPH").unwrap();
    assert_eq!(s.current_schema(), Some("s1".to_string()));
    assert!(s.current_graph().is_none());
}

// ── Identifier validation ───────────────────────────────────────

#[test]
fn create_schema_rejects_slash_in_name() {
    let db = db();
    let s = db.session();

    let result = s.execute("CREATE SCHEMA `foo/bar`");
    assert!(
        result.is_err(),
        "CREATE SCHEMA with '/' in name should fail (reserved separator)"
    );
}

#[test]
fn create_graph_rejects_slash_in_name() {
    let db = db();
    let s = db.session();

    let result = s.execute("CREATE GRAPH `foo/bar`");
    assert!(
        result.is_err(),
        "CREATE GRAPH with '/' in name should fail (reserved separator)"
    );
}

// ── Backward compatibility ──────────────────────────────────────

#[test]
fn no_schema_backward_compat() {
    let db = db();
    let s = db.session();

    // All existing patterns still work without schemas
    s.execute("INSERT (:Person {name: 'Alix'})").unwrap();
    s.execute("CREATE GRAPH g1").unwrap();
    s.execute("USE GRAPH g1").unwrap();
    s.execute("INSERT (:Event {type: 'click'})").unwrap();
    s.execute("USE GRAPH default").unwrap();

    let result = s.execute("MATCH (n:Person) RETURN n").unwrap();
    assert_eq!(result.row_count(), 1, "default graph should still work");
}
