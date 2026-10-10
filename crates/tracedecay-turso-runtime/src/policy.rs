use std::sync::Arc;

use turso_core::{Dialect, Func, LimboError, SqliteDialect};
use turso_parser::ast::{Cmd, PragmaBody, Stmt};

use crate::{Access, Error, Result};

/// Uses the engine's own parser and name resolution. Function policy therefore
/// also applies to stored trigger/view bodies and schema-driven re-preparation.
pub(crate) struct GuardedDialect {
    pinned: bool,
}

impl Dialect for GuardedDialect {
    fn name(&self) -> &'static str {
        if self.pinned {
            "tracedecay-turso-pinned"
        } else {
            "tracedecay-turso"
        }
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
        table: &turso_parser::ast::QualifiedName,
        body: &turso_parser::ast::CreateTableBody,
    ) -> turso_core::Result<String> {
        SqliteDialect.format_table_sql(input, table, body)
    }
    fn register_catalog(
        &self,
        schema: &mut turso_core::schema::Schema,
        custom_types: bool,
    ) -> turso_core::Result<()> {
        SqliteDialect.register_catalog(schema, custom_types)
    }
    fn resolve_function(
        &self,
        name: &str,
        argument_count: usize,
    ) -> turso_core::Result<Option<Func>> {
        if name.eq_ignore_ascii_case("load_extension") {
            return Err(LimboError::ParseError(
                "extension loading is forbidden".to_owned(),
            ));
        }
        SqliteDialect.resolve_function(name, argument_count)
    }
}

pub(crate) fn dialect() -> Arc<dyn Dialect> {
    Arc::new(GuardedDialect { pinned: false })
}

#[cfg(unix)]
pub(crate) fn pinned_dialect() -> Arc<dyn Dialect> {
    // Registry entries from pathname-only opens must never satisfy a
    // descriptor-bound attachment, even when their advertised inode matches.
    Arc::new(GuardedDialect { pinned: true })
}

pub(crate) fn authorize(command: &Cmd, access: Access) -> Result<()> {
    let statement = match command {
        Cmd::Stmt(statement)
        | Cmd::Explain(statement)
        | Cmd::ExplainQueryPlan {
            stmt: statement, ..
        } => statement,
    };
    match statement {
        Stmt::Select(_) => Ok(()),
        Stmt::Pragma { name, body } => authorize_pragma(name.name.as_str(), body.as_ref(), access),
        Stmt::Insert { .. } | Stmt::Delete { .. } | Stmt::Update(_) if access == Access::Writer => {
            Ok(())
        }
        Stmt::CreateTable { .. }
        | Stmt::CreateIndex { .. }
        | Stmt::AlterTable(_)
        | Stmt::DropIndex { .. }
        | Stmt::DropTable { .. }
        | Stmt::Analyze { .. }
        | Stmt::Reindex { .. }
        | Stmt::Optimize { .. }
            if access == Access::Writer =>
        {
            Ok(())
        }
        Stmt::CreateTrigger {
            temporary: false,
            trigger_name,
            ..
        } if access == Access::Writer && !is_temp(trigger_name.db_name.as_ref()) => Ok(()),
        Stmt::CreateView {
            temporary: false,
            view_name,
            ..
        } if access == Access::Writer && !is_temp(view_name.db_name.as_ref()) => Ok(()),
        // Unqualified names search temp first. An explicit main qualification
        // keeps schema lifecycle operations from acquiring temporary authority.
        Stmt::DropTrigger { trigger_name, .. }
            if access == Access::Writer && is_main(trigger_name.db_name.as_ref()) =>
        {
            Ok(())
        }
        Stmt::DropView { view_name, .. }
            if access == Access::Writer && is_main(view_name.db_name.as_ref()) =>
        {
            Ok(())
        }
        Stmt::DropTrigger { .. } | Stmt::DropView { .. } => {
            denied("trigger and view drops require explicit main qualification")
        }
        Stmt::Begin { .. }
        | Stmt::Commit { .. }
        | Stmt::Rollback { .. }
        | Stmt::Savepoint { .. }
        | Stmt::Release { .. } => denied("transactions and savepoints belong to the runtime"),
        Stmt::Attach { .. } | Stmt::Detach { .. } => {
            denied("database attachment belongs to the runtime")
        }
        Stmt::CreateTrigger { .. } | Stmt::CreateView { .. } => {
            denied("temporary triggers and views are forbidden")
        }
        Stmt::Vacuum { .. } => denied("vacuum requires the runtime's file authority"),
        Stmt::CreateVirtualTable(_)
        | Stmt::CreateMaterializedView { .. }
        | Stmt::CreateType { .. }
        | Stmt::CreateDomain { .. }
        | Stmt::DropType { .. }
        | Stmt::DropDomain { .. }
        | Stmt::CreateSequence { .. }
        | Stmt::DropSequence { .. }
        | Stmt::Insert { .. }
        | Stmt::Delete { .. }
        | Stmt::Update(_)
        | Stmt::CreateTable { .. }
        | Stmt::CreateIndex { .. }
        | Stmt::AlterTable(_)
        | Stmt::DropIndex { .. }
        | Stmt::DropTable { .. }
        | Stmt::Analyze { .. }
        | Stmt::Reindex { .. }
        | Stmt::Optimize { .. } => denied("statement is outside this connection capability"),
    }
}

fn is_main(name: Option<&turso_parser::ast::Name>) -> bool {
    name.is_some_and(|name| name.as_str().eq_ignore_ascii_case("main"))
}

fn is_temp(name: Option<&turso_parser::ast::Name>) -> bool {
    name.is_some_and(|name| name.as_str().eq_ignore_ascii_case("temp"))
}

fn authorize_pragma(name: &str, body: Option<&PragmaBody>, access: Access) -> Result<()> {
    // This engine pin has no secure-delete implementation. Unknown pragmas can
    // otherwise succeed without applying anything, which would fabricate a
    // privacy guarantee at an owned-store attachment boundary.
    if name.eq_ignore_ascii_case("secure_delete") {
        return Err(Error::Unsupported(
            "native Turso does not implement secure deletion".to_owned(),
        ));
    }
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
    const READ_ONLY: &[&str] = &[
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
        "synchronous",
        "temp_store",
        "user_version",
        "wal_autocheckpoint",
    ];
    if ARGUMENT_SAFE
        .iter()
        .any(|allowed| name.eq_ignore_ascii_case(allowed))
        || body.is_none()
            && READ_ONLY
                .iter()
                .any(|allowed| name.eq_ignore_ascii_case(allowed))
    {
        return Ok(());
    }
    if access == Access::Writer {
        if body.is_none() && name.eq_ignore_ascii_case("shrink_memory") {
            return Ok(());
        }
        if let Some(PragmaBody::Equals(value) | PragmaBody::Call(value)) = body {
            let value = value.to_string();
            let on = value.eq_ignore_ascii_case("on") || value == "1";
            if (name.eq_ignore_ascii_case("foreign_keys")
                || name.eq_ignore_ascii_case("defer_foreign_keys"))
                && on
                || name.eq_ignore_ascii_case("auto_vacuum")
                    && (value.eq_ignore_ascii_case("incremental") || value == "2")
                || [
                    "busy_timeout",
                    "incremental_vacuum",
                    "user_version",
                    "wal_autocheckpoint",
                ]
                .iter()
                .any(|allowed| name.eq_ignore_ascii_case(allowed))
                    && value.parse::<u32>().is_ok()
            {
                return Ok(());
            }
        }
    }
    denied("pragma would change protected connection or file configuration")
}

fn denied<T>(message: &str) -> Result<T> {
    Err(Error::Denied(message.to_owned()))
}
