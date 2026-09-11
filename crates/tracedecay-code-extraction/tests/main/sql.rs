use tracedecay_code_extraction::{
    ExtractedSchemaFactV1, LanguageExtractor, SchemaEvidenceIssueV1, SchemaEvidenceStatusV1,
    SqlExtractor, SqlSchemaActionV1, SqlSchemaObjectKindV1,
};

#[test]
fn schema_evidence_retains_qualified_ddl_in_statement_order() {
    let source = "CREATE TABLE app.users (id INT);\n\
ALTER TABLE app.users ADD COLUMN email TEXT;\n\
DROP TABLE app.legacy;\n\
CREATE VIEW app.active_users AS SELECT id FROM app.users;\n\
ALTER VIEW app.active_users RENAME TO enabled_users;\n\
DROP VIEW app.enabled_users;\n\
CREATE FUNCTION app.user_count() RETURNS INT AS 'SELECT 1' LANGUAGE SQL;\n\
DROP FUNCTION app.user_count();\n";
    let artifact = SqlExtractor.extract_artifact("migrations/001_users.sql", source);
    let evidence = artifact.schema_evidence.expect("SQL schema evidence");
    assert_eq!(
        evidence.status,
        SchemaEvidenceStatusV1::Partial,
        "{evidence:#?}"
    );
    assert_eq!(evidence.issues, vec![SchemaEvidenceIssueV1::ParseError]);

    let changes = evidence
        .facts
        .iter()
        .filter_map(|fact| match fact {
            ExtractedSchemaFactV1::SqlObjectChange {
                statement_order,
                action,
                object_kind,
                qualified_name,
                span,
            } => Some((
                *statement_order,
                *action,
                *object_kind,
                qualified_name.as_str(),
                &source[span.start_byte as usize..span.end_byte as usize],
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        changes
            .iter()
            .map(|(order, action, kind, name, _)| (*order, *action, *kind, *name))
            .collect::<Vec<_>>(),
        vec![
            (
                0,
                SqlSchemaActionV1::Create,
                SqlSchemaObjectKindV1::Table,
                "app.users"
            ),
            (
                1,
                SqlSchemaActionV1::Alter,
                SqlSchemaObjectKindV1::Table,
                "app.users"
            ),
            (
                2,
                SqlSchemaActionV1::Drop,
                SqlSchemaObjectKindV1::Table,
                "app.legacy"
            ),
            (
                3,
                SqlSchemaActionV1::Create,
                SqlSchemaObjectKindV1::View,
                "app.active_users"
            ),
            (
                4,
                SqlSchemaActionV1::Alter,
                SqlSchemaObjectKindV1::View,
                "app.active_users"
            ),
            (
                5,
                SqlSchemaActionV1::Drop,
                SqlSchemaObjectKindV1::View,
                "app.enabled_users"
            ),
            (
                6,
                SqlSchemaActionV1::Create,
                SqlSchemaObjectKindV1::Function,
                "app.user_count"
            ),
            (
                7,
                SqlSchemaActionV1::Drop,
                SqlSchemaObjectKindV1::Function,
                "app.user_count"
            ),
        ]
    );
    assert!(changes.iter().all(|(_, _, _, _, text)| !text.is_empty()));

    let unsupported_routines = SqlExtractor.extract_artifact(
        "migrations/routines.sql",
        "ALTER FUNCTION app.user_count() RENAME TO count_users;\n\
CREATE PROCEDURE app.refresh() LANGUAGE SQL AS 'SELECT 1';",
    );
    let unsupported_routines = unsupported_routines
        .schema_evidence
        .expect("partial schema evidence");
    assert!(unsupported_routines.facts.is_empty());
    assert_eq!(unsupported_routines.status, SchemaEvidenceStatusV1::Partial);
    assert_eq!(
        unsupported_routines.issues,
        vec![SchemaEvidenceIssueV1::ParseError]
    );

    let dynamic = SqlExtractor.extract_artifact(
        "migrations/dynamic.sql",
        "EXECUTE 'CREATE TABLE app.hidden (id INT)';",
    );
    let dynamic = dynamic
        .schema_evidence
        .expect("unsupported schema evidence");
    assert!(dynamic.facts.is_empty(), "{dynamic:#?}");
    assert_eq!(dynamic.status, SchemaEvidenceStatusV1::Unsupported);
    assert_eq!(
        dynamic.issues,
        vec![
            SchemaEvidenceIssueV1::ParseError,
            SchemaEvidenceIssueV1::UnsupportedSyntax,
        ]
    );
}
