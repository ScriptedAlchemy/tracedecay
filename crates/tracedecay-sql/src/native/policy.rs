use std::sync::Arc;

use turso_core::{Dialect, Func, LimboError, SqliteDialect};
use turso_parser::ast::{Cmd, Name, PragmaBody, QualifiedName, Stmt};

use super::{Error, Result};
use crate::Capability;

/// Prefix of the parse error a denied function resolution raises; the
/// connection maps it to [`Error::Denied`].
pub(crate) const DENIED_FUNCTION: &str = "native SQL denied function: ";

/// Built-ins whose effects reach beyond the statement's own rows: extension
/// loading, sequence state, and engine test hooks.
const DENIED_FUNCTIONS: &[&str] = &[
    "load_extension",
    "nextval",
    "setval",
    "currval",
    "sequence_watermark_experimental",
];

/// The only catalog objects SQL can name: JSON table functions and the
/// read-only introspection pragmas. `sqlite_dbpage`, `btree_dump` and
/// `turso_types` never resolve, whatever features the engine is built with.
const CATALOG: &[&str] = &[
    "json_each",
    "json_tree",
    "pragma_collation_list",
    "pragma_database_list",
    "pragma_foreign_key_check",
    "pragma_foreign_key_list",
    "pragma_freelist_count",
    "pragma_function_list",
    "pragma_index_info",
    "pragma_index_list",
    "pragma_index_xinfo",
    "pragma_integrity_check",
    "pragma_module_list",
    "pragma_page_count",
    "pragma_pragma_list",
    "pragma_quick_check",
    "pragma_table_info",
    "pragma_table_list",
    "pragma_table_xinfo",
];

/// Uses the engine's own parser and name resolution, so function policy also
/// applies to trigger and view bodies and to schema-driven re-preparation.
pub(crate) struct GuardedDialect;

impl Dialect for GuardedDialect {
    fn name(&self) -> &'static str {
        "tracedecay-turso"
    }
    fn parse(&self, sql: &str) -> turso_core::Result<(Option<Cmd>, usize)> {
        SqliteDialect.parse(sql)
    }
    fn parse_table_sql(
        &self,
        sql: &str,
        root_page: i64,
    ) -> turso_core::Result<turso_core::schema::BTreeTable> {
        SqliteDialect.parse_table_sql(sql, root_page)
    }
    fn parse_table_sql_ast(&self, sql: &str) -> turso_core::Result<Stmt> {
        SqliteDialect.parse_table_sql_ast(sql)
    }
    fn table_sql_for_replay(&self, sql: &str) -> turso_core::Result<String> {
        SqliteDialect.table_sql_for_replay(sql)
    }
    fn format_table_sql(
        &self,
        input: &str,
        table: &QualifiedName,
        body: &turso_parser::ast::CreateTableBody,
    ) -> turso_core::Result<String> {
        SqliteDialect.format_table_sql(input, table, body)
    }
    fn register_catalog(
        &self,
        schema: &mut turso_core::schema::Schema,
        _custom_types: bool,
    ) -> turso_core::Result<()> {
        let existing: Vec<String> = schema.tables.keys().cloned().collect();
        SqliteDialect.register_catalog(schema, false)?;
        schema
            .tables
            .retain(|name, _| existing.contains(name) || CATALOG.contains(&name.as_str()));
        Ok(())
    }
    fn resolve_function(
        &self,
        name: &str,
        argument_count: usize,
    ) -> turso_core::Result<Option<Func>> {
        if name.to_ascii_lowercase().starts_with("test_")
            || DENIED_FUNCTIONS
                .iter()
                .any(|denied| name.eq_ignore_ascii_case(denied))
        {
            return Err(LimboError::ParseError(format!("{DENIED_FUNCTION}{name}")));
        }
        SqliteDialect.resolve_function(name, argument_count)
    }
}

pub(crate) fn dialect() -> Arc<dyn Dialect> {
    Arc::new(GuardedDialect)
}

fn statement(command: &Cmd) -> &Stmt {
    match command {
        Cmd::Stmt(statement)
        | Cmd::Explain(statement)
        | Cmd::ExplainQueryPlan {
            stmt: statement, ..
        } => statement,
    }
}

/// Whether running `command` changes the schema, which invalidates cached
/// statements.
pub(crate) fn is_ddl(command: &Cmd) -> bool {
    matches!(command, Cmd::Stmt(_))
        && matches!(
            statement(command),
            Stmt::AlterTable(_)
                | Stmt::CreateIndex { .. }
                | Stmt::CreateTable { .. }
                | Stmt::CreateTrigger { .. }
                | Stmt::CreateView { .. }
                | Stmt::CreateVirtualTable(_)
                | Stmt::DropIndex { .. }
                | Stmt::DropTable { .. }
                | Stmt::DropTrigger { .. }
                | Stmt::DropView { .. }
        )
}

/// The AST verdict: a pure function of the parsed statement and the
/// capability, mirroring the SQLite arm's authorizers. `EXPLAIN` is judged by
/// the statement it explains.
pub(crate) fn authorize(command: &Cmd, capability: Capability) -> Result<()> {
    use Capability::{ExactSql, ExactSqlRead, Maintenance, Reader, Writer};

    let temp = |name: &QualifiedName| is_temp(name.db_name.as_ref());
    let allowed = match statement(command) {
        Stmt::Select(_) => true,
        Stmt::Pragma { name, body } => {
            return authorize_pragma(name.name.as_str(), body.as_ref(), capability);
        }
        Stmt::Begin { .. }
        | Stmt::Commit { .. }
        | Stmt::Rollback {
            savepoint_name: None,
            ..
        } => return authorize_transaction_control(capability, false),
        Stmt::Rollback { .. } | Stmt::Savepoint { .. } | Stmt::Release { .. } => {
            return authorize_transaction_control(capability, true);
        }
        Stmt::Insert { .. } | Stmt::Delete { .. } | Stmt::Update(_) | Stmt::Reindex { .. } => {
            !read_only(capability)
        }
        Stmt::CreateTable {
            temporary,
            tbl_name,
            ..
        } => match capability {
            Reader | ExactSqlRead => false,
            Writer => !temporary && !temp(tbl_name),
            ExactSql { .. } | Maintenance => true,
        },
        Stmt::CreateIndex { idx_name, .. } => match capability {
            Reader | ExactSqlRead => false,
            Writer => !temp(idx_name),
            ExactSql { .. } | Maintenance => true,
        },
        // A temporary trigger can fire on a durable table outside the
        // canonical trigger contract, so no capability may create one.
        Stmt::CreateTrigger {
            temporary,
            trigger_name: name,
            ..
        }
        | Stmt::CreateView {
            temporary,
            view_name: name,
            ..
        } => durable_schema(capability) && !temporary && !temp(name),
        Stmt::DropTrigger {
            trigger_name: name, ..
        }
        | Stmt::DropView {
            view_name: name, ..
        } => durable_schema(capability) && !temp(name),
        Stmt::AlterTable(_)
        | Stmt::Analyze { .. }
        | Stmt::CreateVirtualTable(_)
        | Stmt::DropIndex { .. }
        | Stmt::DropTable { .. }
        | Stmt::Optimize { .. } => durable_schema(capability),
        // Attachment, in-place VACUUM and the engine's non-SQLite object
        // kinds have no owned-store caller under any capability.
        Stmt::Attach { .. }
        | Stmt::Detach { .. }
        | Stmt::Vacuum { .. }
        | Stmt::CreateMaterializedView { .. }
        | Stmt::CreateType { .. }
        | Stmt::CreateDomain { .. }
        | Stmt::DropType { .. }
        | Stmt::DropDomain { .. }
        | Stmt::CreateSequence { .. }
        | Stmt::DropSequence { .. } => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(Error::Denied(format!(
            "statement is outside the {capability:?} capability"
        )))
    }
}

/// Triggers, views, drops and other schema surgery beyond the ledger
/// bootstrap's `CREATE TABLE`/`CREATE INDEX`.
fn durable_schema(capability: Capability) -> bool {
    match capability {
        Capability::Reader | Capability::ExactSqlRead | Capability::Writer => false,
        Capability::ExactSql { .. } | Capability::Maintenance => true,
    }
}

fn read_only(capability: Capability) -> bool {
    matches!(capability, Capability::Reader | Capability::ExactSqlRead)
}

/// Caller-supplied SQL never begins or ends a transaction: the runtime owns
/// it. Savepoints are allowed only under a transaction lease.
fn authorize_transaction_control(capability: Capability, savepoint: bool) -> Result<()> {
    match capability {
        Capability::ExactSql { savepoints: true } if savepoint => Ok(()),
        Capability::ExactSqlRead | Capability::ExactSql { .. } => {
            Err(Error::TransactionControlDenied)
        }
        Capability::Reader | Capability::Writer | Capability::Maintenance => Ok(()),
    }
}

fn is_temp(name: Option<&Name>) -> bool {
    name.is_some_and(|name| name.as_str().eq_ignore_ascii_case("temp"))
}

/// Introspection pragmas any capability may run with an argument.
const INTROSPECTION: &[&str] = &[
    "collation_list",
    "database_list",
    "foreign_key_check",
    "foreign_key_list",
    "function_list",
    "index_info",
    "index_list",
    "index_xinfo",
    "integrity_check",
    "module_list",
    "pragma_list",
    "quick_check",
    "table_info",
    "table_list",
    "table_xinfo",
];

fn authorize_pragma(name: &str, body: Option<&PragmaBody>, capability: Capability) -> Result<()> {
    let is = |candidate: &&str| name.eq_ignore_ascii_case(candidate);
    // The engine has no secure-delete implementation; an unknown pragma can
    // succeed without applying anything, which would fabricate a privacy
    // guarantee.
    if is(&"secure_delete") {
        return Err(Error::Unsupported(
            "native Turso does not implement secure deletion".to_owned(),
        ));
    }
    let value = body.map(|(PragmaBody::Equals(value) | PragmaBody::Call(value))| {
        value
            .to_string()
            .trim_matches(|c| c == '\'' || c == '"')
            .to_owned()
    });
    // Commit durability is the driver's: every connection fsyncs on commit.
    if is(&"synchronous") && value.is_some() {
        return Err(Error::Denied(
            "the native driver fixes synchronous at FULL".to_owned(),
        ));
    }
    let allowed = match (capability, value.as_deref()) {
        (Capability::ExactSql { .. }, value) => is_allowed_exact_sql_pragma(name, value),
        (_, None) => true,
        (_, Some(value)) => {
            capability == Capability::Maintenance
                || INTROSPECTION.iter().any(is)
                || (capability == Capability::Writer && is_safe_writer_pragma(name, value))
        }
    };
    if allowed {
        Ok(())
    } else {
        Err(Error::Denied(
            "pragma would change protected connection or file configuration".to_owned(),
        ))
    }
}

fn is_safe_writer_pragma(name: &str, value: &str) -> bool {
    [
        "busy_timeout",
        "incremental_vacuum",
        "wal_autocheckpoint",
        "wal_checkpoint",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
        || (name.eq_ignore_ascii_case("auto_vacuum")
            && (value.eq_ignore_ascii_case("incremental") || value == "2"))
}

/// The SQLite arm's exact SQL pragma allowlist (`sqlite.rs`).
fn is_allowed_exact_sql_pragma(name: &str, value: Option<&str>) -> bool {
    const ARGUMENT_SAFE: &[&str] = &[
        "foreign_key_check",
        "foreign_key_list",
        "index_info",
        "index_list",
        "index_xinfo",
        "integrity_check",
        "quick_check",
        "table_info",
        "table_list",
        "table_xinfo",
    ];
    const NO_ARGUMENT_ONLY: &[&str] = &[
        "application_id",
        "auto_vacuum",
        "busy_timeout",
        "cache_size",
        "collation_list",
        "compile_options",
        "data_version",
        "database_list",
        "defer_foreign_keys",
        "foreign_keys",
        "freelist_count",
        "function_list",
        "journal_mode",
        "mmap_size",
        "module_list",
        "page_count",
        "page_size",
        "pragma_list",
        "query_only",
        "recursive_triggers",
        "schema_version",
        "secure_delete",
        "synchronous",
        "temp_store",
        "user_version",
        "wal_autocheckpoint",
    ];
    const SET_FLAG: &[&str] = &["foreign_keys", "defer_foreign_keys"];
    const SET_COUNT: &[&str] = &[
        "busy_timeout",
        "incremental_vacuum",
        "user_version",
        "wal_autocheckpoint",
    ];
    let is = |candidate: &&str| name.eq_ignore_ascii_case(candidate);
    match value {
        _ if ARGUMENT_SAFE.iter().any(is) => true,
        None => NO_ARGUMENT_ONLY.iter().any(is) || is(&"shrink_memory"),
        Some(value) => {
            (is(&"auto_vacuum") && (value.eq_ignore_ascii_case("incremental") || value == "2"))
                || (SET_FLAG.iter().any(is) && (value.eq_ignore_ascii_case("on") || value == "1"))
                || (SET_COUNT.iter().any(is) && value.parse::<u32>().is_ok())
        }
    }
}
