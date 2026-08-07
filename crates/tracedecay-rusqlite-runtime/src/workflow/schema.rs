//! Workflow source-journal, effect-journal, and handoff tables on the registered writer.

use rusqlite::Connection;

pub const WORKFLOW_SCHEMA_VERSION_V1: i64 = 1;
pub const WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1: &str =
    "sha256:f954b3be07e9397b71e6025a7635da202b8a3a89d681f90f463034133d33ffec";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkflowColumnContractV1 {
    pub name: &'static str,
    pub sql_type: &'static str,
    pub not_null: i64,
    pub primary_key: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkflowTableContractV1 {
    pub name: &'static str,
    pub sql: &'static str,
    pub columns: &'static [WorkflowColumnContractV1],
}

const WORKFLOW_DEFINITION_SOURCE_COLUMNS_V1: &[WorkflowColumnContractV1] = &[
    WorkflowColumnContractV1 {
        name: "definition_id",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 1,
    },
    WorkflowColumnContractV1 {
        name: "definition_version",
        sql_type: "INTEGER",
        not_null: 1,
        primary_key: 2,
    },
    WorkflowColumnContractV1 {
        name: "payload",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "payload_digest",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 0,
    },
];

const WORKFLOW_EFFECT_COLUMNS_V1: &[WorkflowColumnContractV1] = &[
    WorkflowColumnContractV1 {
        name: "idempotency_key",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 1,
    },
    WorkflowColumnContractV1 {
        name: "identity_digest",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "identity_payload",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "identity_payload_digest",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "prepared_payload",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "prepared_payload_digest",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "operation",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "state",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "terminal_payload",
        sql_type: "TEXT",
        not_null: 0,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "terminal_payload_digest",
        sql_type: "TEXT",
        not_null: 0,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "created_at",
        sql_type: "INTEGER",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "updated_at",
        sql_type: "INTEGER",
        not_null: 1,
        primary_key: 0,
    },
];

const WORKFLOW_HANDOFF_COLUMNS_V1: &[WorkflowColumnContractV1] = &[
    WorkflowColumnContractV1 {
        name: "token_digest",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 1,
    },
    WorkflowColumnContractV1 {
        name: "scope_payload",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "issued_at",
        sql_type: "INTEGER",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "expires_at",
        sql_type: "INTEGER",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "consumed",
        sql_type: "INTEGER",
        not_null: 1,
        primary_key: 0,
    },
];

const WORKFLOW_SCHEMA_COLUMNS_V1: &[WorkflowColumnContractV1] = &[
    WorkflowColumnContractV1 {
        name: "singleton",
        sql_type: "INTEGER",
        not_null: 1,
        primary_key: 1,
    },
    WorkflowColumnContractV1 {
        name: "schema_version",
        sql_type: "INTEGER",
        not_null: 1,
        primary_key: 0,
    },
    WorkflowColumnContractV1 {
        name: "definition_digest",
        sql_type: "TEXT",
        not_null: 1,
        primary_key: 0,
    },
];

const WORKFLOW_DEFINITION_SOURCE_JOURNAL_SQL_V1: &str =
    "CREATE TABLE workflow_definition_source_journal (
    definition_id TEXT NOT NULL,
    definition_version INTEGER NOT NULL CHECK (definition_version > 0),
    payload TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    PRIMARY KEY (definition_id, definition_version)
) STRICT";

const WORKFLOW_EFFECT_JOURNAL_SQL_V1: &str = "CREATE TABLE workflow_effect_journal (
    idempotency_key TEXT NOT NULL PRIMARY KEY,
    identity_digest TEXT NOT NULL,
    identity_payload TEXT NOT NULL,
    identity_payload_digest TEXT NOT NULL,
    prepared_payload TEXT NOT NULL,
    prepared_payload_digest TEXT NOT NULL,
    operation TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN ('before_effect', 'in_flight', 'committed', 'reconciled')
    ),
    terminal_payload TEXT,
    terminal_payload_digest TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT";

const WORKFLOW_HANDOFFS_SQL_V1: &str = "CREATE TABLE workflow_handoffs (
    token_digest TEXT NOT NULL PRIMARY KEY,
    scope_payload TEXT NOT NULL,
    issued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > issued_at),
    consumed INTEGER NOT NULL CHECK (consumed IN (0, 1))
) STRICT";

const WORKFLOW_SCHEMA_SQL_V1: &str = "CREATE TABLE workflow_schema (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    definition_digest TEXT NOT NULL
) STRICT";

pub const WORKFLOW_SCHEMA_IDENTITY_V1: &str =
    "INSERT INTO workflow_schema (singleton, schema_version, definition_digest)
VALUES (
    1,
    1,
    'sha256:f954b3be07e9397b71e6025a7635da202b8a3a89d681f90f463034133d33ffec'
)";

pub const WORKFLOW_TABLE_CONTRACTS_V1: &[WorkflowTableContractV1] = &[
    WorkflowTableContractV1 {
        name: "workflow_definition_source_journal",
        sql: WORKFLOW_DEFINITION_SOURCE_JOURNAL_SQL_V1,
        columns: WORKFLOW_DEFINITION_SOURCE_COLUMNS_V1,
    },
    WorkflowTableContractV1 {
        name: "workflow_effect_journal",
        sql: WORKFLOW_EFFECT_JOURNAL_SQL_V1,
        columns: WORKFLOW_EFFECT_COLUMNS_V1,
    },
    WorkflowTableContractV1 {
        name: "workflow_handoffs",
        sql: WORKFLOW_HANDOFFS_SQL_V1,
        columns: WORKFLOW_HANDOFF_COLUMNS_V1,
    },
    WorkflowTableContractV1 {
        name: "workflow_schema",
        sql: WORKFLOW_SCHEMA_SQL_V1,
        columns: WORKFLOW_SCHEMA_COLUMNS_V1,
    },
];

pub fn install_workflow_schema(connection: &Connection) -> rusqlite::Result<()> {
    let mut sql = String::new();
    for table in WORKFLOW_TABLE_CONTRACTS_V1 {
        sql.push_str(table.sql);
        sql.push_str(";\n");
    }
    sql.push_str(WORKFLOW_SCHEMA_IDENTITY_V1);
    connection.execute_batch(&sql)
}
