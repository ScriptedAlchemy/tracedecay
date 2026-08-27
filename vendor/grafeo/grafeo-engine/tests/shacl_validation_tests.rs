//! SHACL Validation integration tests.
//!
//! Tests the full validation pipeline: SPARQL INSERT DATA → shape parsing →
//! target resolution → constraint evaluation → ValidationReport.

#![cfg(all(feature = "triple-store", feature = "shacl"))]

use grafeo_engine::GrafeoDB;
use grafeo_engine::config::{Config, GraphModel};

fn rdf_db() -> GrafeoDB {
    GrafeoDB::with_config(Config::in_memory().with_graph_model(GraphModel::Rdf)).unwrap()
}

/// Helper: inserts data into the default graph and shapes into a named graph.
fn setup_validation(data_sparql: &str, shapes_sparql: &str) -> GrafeoDB {
    let db = rdf_db();
    let session = db.session();
    if !data_sparql.is_empty() {
        session.execute_sparql(data_sparql).unwrap();
    }
    if !shapes_sparql.is_empty() {
        session.execute_sparql(shapes_sparql).unwrap();
    }
    db
}

const SHAPES_GRAPH: &str = "http://ex.org/shapes";

// =========================================================================
// A. Value Type Constraints
// =========================================================================

#[test]
fn class_valid() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> .
            <http://ex.org/alix> <http://ex.org/knows> <http://ex.org/gus> .
            <http://ex.org/gus> a <http://ex.org/Person> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/knows> ;
                    <http://www.w3.org/ns/shacl#class> <http://ex.org/Person>
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "All known persons are Person: {report}");
}

#[test]
fn class_invalid() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> .
            <http://ex.org/alix> <http://ex.org/knows> <http://ex.org/amsterdam> .
            <http://ex.org/amsterdam> a <http://ex.org/City> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/knows> ;
                    <http://www.w3.org/ns/shacl#class> <http://ex.org/Person>
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms);
    assert!(
        report
            .results
            .iter()
            .any(|r| r.source_constraint_component.contains("Class"))
    );
}

#[test]
fn datatype_valid() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/age> "30"^^<http://www.w3.org/2001/XMLSchema#integer> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/age> ;
                    <http://www.w3.org/ns/shacl#datatype> <http://www.w3.org/2001/XMLSchema#integer>
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "{report}");
}

#[test]
fn datatype_mismatch() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/age> "thirty" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/age> ;
                    <http://www.w3.org/ns/shacl#datatype> <http://www.w3.org/2001/XMLSchema#integer>
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms);
}

#[test]
fn node_kind_iri() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/knows> <http://ex.org/gus> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/knows> ;
                    <http://www.w3.org/ns/shacl#nodeKind> <http://www.w3.org/ns/shacl#IRI>
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "{report}");
}

#[test]
fn node_kind_literal_on_iri_fails() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/knows> <http://ex.org/gus> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/knows> ;
                    <http://www.w3.org/ns/shacl#nodeKind> <http://www.w3.org/ns/shacl#Literal>
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms);
}

// =========================================================================
// B. Cardinality Constraints
// =========================================================================

#[test]
fn mincount_pass() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/name> "Alix" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#minCount> 1
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "{report}");
}

#[test]
fn mincount_violation() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#minCount> 1
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms);
    assert_eq!(report.results.len(), 1);
    assert!(
        report.results[0]
            .source_constraint_component
            .contains("MinCount")
    );
}

#[test]
fn maxcount_pass() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/name> "Alix" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#maxCount> 1
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "{report}");
}

#[test]
fn maxcount_violation() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/name> "Alix" ;
                <http://ex.org/name> "Alex" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#maxCount> 1
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms);
}

// =========================================================================
// C. Value Range Constraints
// =========================================================================

#[test]
fn min_inclusive_pass() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/age> "30"^^<http://www.w3.org/2001/XMLSchema#integer> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/age> ;
                    <http://www.w3.org/ns/shacl#minInclusive> "18"^^<http://www.w3.org/2001/XMLSchema#integer>
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "{report}");
}

#[test]
fn min_inclusive_fail() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/age> "15"^^<http://www.w3.org/2001/XMLSchema#integer> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/age> ;
                    <http://www.w3.org/ns/shacl#minInclusive> "18"^^<http://www.w3.org/2001/XMLSchema#integer>
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms);
}

// =========================================================================
// D. String Constraints
// =========================================================================

#[test]
fn minlength_pass() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/name> "Alix" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#minLength> 3
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "{report}");
}

#[test]
fn minlength_fail() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/name> "Al" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#minLength> 3
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms);
}

#[test]
fn pattern_match() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/email> "alix@example.org" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/email> ;
                    <http://www.w3.org/ns/shacl#pattern> "^[^@]+@[^@]+$"
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "{report}");
}

#[test]
fn pattern_no_match() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/email> "not-an-email" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/email> ;
                    <http://www.w3.org/ns/shacl#pattern> "^[^@]+@[^@]+$"
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms);
}

// =========================================================================
// E. Property Pair Constraints
// =========================================================================

#[test]
fn equals_match() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/name> "Alix" ;
                <http://ex.org/label> "Alix" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#equals> <http://ex.org/label>
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "{report}");
}

#[test]
fn disjoint_overlap() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/name> "Alix" ;
                <http://ex.org/nickname> "Alix" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#disjoint> <http://ex.org/nickname>
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms);
}

// =========================================================================
// F. Other Constraints
// =========================================================================

#[test]
fn has_value_present() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/status> "active" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/status> ;
                    <http://www.w3.org/ns/shacl#hasValue> "active"
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "{report}");
}

#[test]
fn has_value_absent() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/status> "inactive" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/status> ;
                    <http://www.w3.org/ns/shacl#hasValue> "active"
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms);
}

// =========================================================================
// G. Shape Metadata
// =========================================================================

#[test]
fn deactivated_shape_skipped() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#deactivated> true ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#minCount> 1
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "Deactivated shape should be skipped");
}

#[test]
fn multiple_shapes_partial_conformance() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/name> "Alix" .
            <http://ex.org/gus> a <http://ex.org/Person> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#minCount> 1
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms, "Gus is missing name");
    // Only one violation (gus), alix conforms
    assert_eq!(report.results.len(), 1);
}

// =========================================================================
// H. End-to-End
// =========================================================================

#[test]
fn empty_data_conforms() {
    let db = setup_validation(
        "",
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#minCount> 1
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "No targets = no violations");
}

#[test]
fn empty_shapes_conforms() {
    let db = rdf_db();
    let session = db.session();
    session
        .execute_sparql(
            r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> .
        }"#,
        )
        .unwrap();
    // Create an empty shapes graph
    session
        .execute_sparql(&format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{ <http://ex.org/dummy> <http://ex.org/p> "x" . }} }}"#
        ))
        .unwrap();
    session
        .execute_sparql(&format!(
            r#"DELETE DATA {{ GRAPH <{SHAPES_GRAPH}> {{ <http://ex.org/dummy> <http://ex.org/p> "x" . }} }}"#
        ))
        .unwrap();
    let report = session.validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "No shapes = conforms");
}

#[test]
fn full_person_scenario() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/name> "Alix" ;
                <http://ex.org/age> "30"^^<http://www.w3.org/2001/XMLSchema#integer> ;
                <http://ex.org/email> "alix@example.org" .
            <http://ex.org/gus> a <http://ex.org/Person> ;
                <http://ex.org/name> "Gus" ;
                <http://ex.org/age> "12"^^<http://www.w3.org/2001/XMLSchema#integer> ;
                <http://ex.org/email> "bad-email" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/PersonShape> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#minCount> 1 ;
                    <http://www.w3.org/ns/shacl#maxCount> 1
                ] ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/age> ;
                    <http://www.w3.org/ns/shacl#datatype> <http://www.w3.org/2001/XMLSchema#integer> ;
                    <http://www.w3.org/ns/shacl#minInclusive> "18"^^<http://www.w3.org/2001/XMLSchema#integer>
                ] ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/email> ;
                    <http://www.w3.org/ns/shacl#pattern> "^[^@]+@[^@]+$"
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms);
    // Gus has two violations: age < 18 and bad email
    let gus_violations: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.focus_node.to_string().contains("gus"))
        .collect();
    assert!(
        gus_violations.len() >= 2,
        "Expected at least 2 violations for gus, got {}",
        gus_violations.len()
    );
}

#[test]
fn report_display_format() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#minCount> 1
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    let text = format!("{report}");
    assert!(text.contains("FAILED"));
    assert!(text.contains("Violation"));
}

#[test]
fn report_to_triples_roundtrip() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#minCount> 1
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    let triples = report.to_triples();
    // Report node + conforms + at least one result with its properties
    assert!(
        triples.len() >= 8,
        "Expected >= 8 triples for report with 1 violation, got {}",
        triples.len()
    );
}

#[test]
fn nonexistent_shapes_graph_errors() {
    let db = rdf_db();
    let session = db.session();
    let result = session.validate_shacl("http://ex.org/nonexistent");
    assert!(result.is_err());
}

// =========================================================================
// validate_shacl_graph (named data graph + named shapes graph)
// =========================================================================

#[test]
fn validate_shacl_graph_both_named() {
    let db = rdf_db();
    let session = db.session();

    session
        .execute_sparql(
            r#"INSERT DATA {
                GRAPH <http://ex.org/data> {
                    <http://ex.org/alix> a <http://ex.org/Person> ;
                        <http://ex.org/name> "Alix" .
                }
            }"#,
        )
        .unwrap();

    session
        .execute_sparql(&format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
                <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                    <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                    <http://www.w3.org/ns/shacl#property> [
                        <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                        <http://www.w3.org/ns/shacl#minCount> 1
                    ] .
            }} }}"#
        ))
        .unwrap();

    let report = session
        .validate_shacl_graph("http://ex.org/data", SHAPES_GRAPH)
        .unwrap();
    assert!(report.conforms, "Alix has a name: {report}");
}

#[test]
fn validate_shacl_graph_violation_in_named_graph() {
    let db = rdf_db();
    let session = db.session();

    session
        .execute_sparql(
            r#"INSERT DATA {
                GRAPH <http://ex.org/data> {
                    <http://ex.org/gus> a <http://ex.org/Person> .
                }
            }"#,
        )
        .unwrap();

    session
        .execute_sparql(&format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
                <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                    <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                    <http://www.w3.org/ns/shacl#property> [
                        <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                        <http://www.w3.org/ns/shacl#minCount> 1
                    ] .
            }} }}"#
        ))
        .unwrap();

    let report = session
        .validate_shacl_graph("http://ex.org/data", SHAPES_GRAPH)
        .unwrap();
    assert!(!report.conforms, "Gus missing name in named data graph");
    assert_eq!(report.results.len(), 1);
}

#[test]
fn validate_shacl_graph_nonexistent_data_graph() {
    let db = rdf_db();
    let session = db.session();

    // Create shapes graph
    session
        .execute_sparql(&format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
                <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> .
            }} }}"#
        ))
        .unwrap();

    let result = session.validate_shacl_graph("http://ex.org/nonexistent", SHAPES_GRAPH);
    assert!(result.is_err());
}

// =========================================================================
// GrafeoDB convenience method
// =========================================================================

#[test]
fn grafeo_db_validate_shacl() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/name> "Alix" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#minCount> 1
                ] .
        }} }}"#
        ),
    );
    let report = db.validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "{report}");
}

// =========================================================================
// Additional constraint coverage via integration
// =========================================================================

#[test]
fn node_kind_blank_node_or_iri() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/knows> <http://ex.org/gus> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/knows> ;
                    <http://www.w3.org/ns/shacl#nodeKind> <http://www.w3.org/ns/shacl#BlankNodeOrIRI>
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(report.conforms, "IRI matches BlankNodeOrIRI: {report}");
}

#[test]
fn max_exclusive_pass_and_fail() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/age> "30"^^<http://www.w3.org/2001/XMLSchema#integer> .
            <http://ex.org/gus> a <http://ex.org/Person> ;
                <http://ex.org/age> "100"^^<http://www.w3.org/2001/XMLSchema#integer> .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/age> ;
                    <http://www.w3.org/ns/shacl#maxExclusive> "100"^^<http://www.w3.org/2001/XMLSchema#integer>
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms, "100 is not < 100");
    assert_eq!(
        report.results.len(),
        1,
        "Only gus (100) should fail maxExclusive 100"
    );
}

#[test]
fn maxlength_pass_and_fail() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/name> "Alix" .
            <http://ex.org/gus> a <http://ex.org/Person> ;
                <http://ex.org/name> "Gustav von Trapp" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#maxLength> 10
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(!report.conforms);
    assert_eq!(
        report.results.len(),
        1,
        "Only Gustav von Trapp exceeds maxLength 10"
    );
}

#[test]
fn closed_shape_integration() {
    let db = setup_validation(
        r#"INSERT DATA {
            <http://ex.org/alix> a <http://ex.org/Person> ;
                <http://ex.org/name> "Alix" ;
                <http://ex.org/secret> "hidden" .
        }"#,
        &format!(
            r#"INSERT DATA {{ GRAPH <{SHAPES_GRAPH}> {{
            <http://ex.org/S> a <http://www.w3.org/ns/shacl#NodeShape> ;
                <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/Person> ;
                <http://www.w3.org/ns/shacl#closed> true ;
                <http://www.w3.org/ns/shacl#property> [
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ;
                    <http://www.w3.org/ns/shacl#minCount> 1
                ] .
        }} }}"#
        ),
    );
    let report = db.session().validate_shacl(SHAPES_GRAPH).unwrap();
    assert!(
        !report.conforms,
        "secret property should violate closed shape"
    );
    assert!(
        report
            .results
            .iter()
            .any(|r| r.source_constraint_component.contains("Closed")),
        "Should have a Closed violation"
    );
}
