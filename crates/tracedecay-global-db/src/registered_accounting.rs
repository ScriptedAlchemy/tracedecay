use std::path::Path;

use tracedecay_runtime_core::db::engine::Value;

use super::{
    RegisteredGlobalDb, SavingsDay, SavingsTotal, global_db_operation_error,
    push_optional_analytics_filter,
};

impl RegisteredGlobalDb {
    #[hotpath::measure(future = true, label = "global_db.registered.accounting.upsert")]
    pub async fn try_upsert_project_tokens(
        &self,
        project_path: &Path,
        tokens_saved: u64,
    ) -> tracedecay_domain::errors::Result<()> {
        crate::hotpath_observe::record_transaction_rows(1);
        let path = super::project_path_alias_key(project_path);
        let transaction = self.begin_write_transaction().await?;
        transaction
            .execute(
                "INSERT INTO projects (path, tokens_saved) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET
                    tokens_saved = MAX(tokens_saved, excluded.tokens_saved)",
                tracedecay_runtime_core::db::engine::params![path, tokens_saved as i64],
            )
            .await
            .map_err(|error| global_db_operation_error("update project token total", error))?;
        transaction
            .commit()
            .await
            .map_err(|error| global_db_operation_error("commit project token total", error))
    }

    /// A project with no registry row has genuinely saved nothing, so an
    /// absent row is `Ok(0)`. Every other outcome is a failed read and stays
    /// an error rather than becoming that same zero.
    #[hotpath::skip]
    pub async fn try_get_project_tokens(&self, project_path: &Path) -> Result<u64, String> {
        self.try_tokens_saved(Some(project_path), "project").await
    }

    #[hotpath::skip]
    pub async fn try_global_tokens_saved(&self) -> Result<u64, String> {
        self.try_tokens_saved(None, "global").await
    }

    #[hotpath::skip]
    async fn try_tokens_saved(
        &self,
        project_path: Option<&Path>,
        scope: &str,
    ) -> Result<u64, String> {
        let path = project_path.map(super::project_path_alias_key);
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to open accounting snapshot: {error}"))?;
        let (sql, values) = tokens_saved_query(path.as_deref());
        let mut rows = snapshot
            .query(&sql, values)
            .await
            .map_err(|error| format!("failed to query {scope} tokens saved: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read {scope} tokens saved row: {error}"))?
            .ok_or_else(|| format!("{scope} tokens saved query returned no row"))?;
        let total = row
            .get::<i64>(0)
            .map_err(|error| format!("failed to decode {scope} tokens saved: {error}"))?;
        u64::try_from(total)
            .map_err(|_| format!("{scope} tokens saved cannot be negative: {total}"))
    }

    #[hotpath::measure(future = true, label = "global_db.registered.accounting.record")]
    pub async fn try_record_savings(
        &self,
        project_path: &str,
        tool_name: &str,
        before_tokens: u64,
        after_tokens: u64,
        timestamp: i64,
    ) -> tracedecay_domain::errors::Result<()> {
        crate::hotpath_observe::record_transaction_rows(1);
        let project_path = RegisteredGlobalDb::canonical_project_key(Path::new(project_path));
        let transaction = self.begin_write_transaction().await?;
        transaction
            .execute(
                "INSERT INTO savings_ledger
                     (ts, project_path, tool_name, before_tokens, after_tokens)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                tracedecay_runtime_core::db::engine::params![
                    timestamp,
                    project_path,
                    tool_name,
                    before_tokens as i64,
                    after_tokens as i64
                ],
            )
            .await
            .map_err(|error| global_db_operation_error("append savings ledger entry", error))?;
        transaction
            .commit()
            .await
            .map_err(|error| global_db_operation_error("commit savings ledger entry", error))
    }

    /// Sums settled savings-ledger rows for an optional project path.
    /// A failed read stays an error instead of becoming a trustworthy zero.
    #[hotpath::skip]
    pub async fn sum_savings(
        &self,
        project: Option<&str>,
        since: i64,
    ) -> Result<SavingsTotal, String> {
        let project =
            project.map(|path| RegisteredGlobalDb::canonical_project_key(Path::new(path)));
        self.sum_savings_by_project_id(project.as_deref(), since)
            .await
    }

    /// Same aggregation for an already-resolved canonical project identity.
    /// Application read models use this to avoid reinterpreting identity as a path.
    #[hotpath::skip]
    pub async fn sum_savings_by_project_id(
        &self,
        project_id: Option<&str>,
        since: i64,
    ) -> Result<SavingsTotal, String> {
        self.savings_totals_with_watermark(project_id, since)
            .await
            .map(|(totals, _)| totals)
    }

    #[hotpath::measure(future = true, label = "global_db.registered.accounting.totals")]
    pub async fn savings_totals_with_watermark(
        &self,
        project_id: Option<&str>,
        since: i64,
    ) -> Result<(SavingsTotal, i64), String> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin savings snapshot: {error}"))?;
        let (sql, values) = savings_scope_query(
            "SELECT COALESCE(SUM(CASE
                    WHEN before_tokens > after_tokens
                    THEN before_tokens - after_tokens
                    ELSE 0 END), 0),
                    COUNT(*),
                    COALESCE(MAX(id), 0)
             FROM savings_ledger",
            project_id,
            since,
        );
        let mut rows = snapshot
            .query(&sql, values)
            .await
            .map_err(|error| format!("failed to query savings totals: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read savings totals: {error}"))?
            .ok_or_else(|| "savings totals query returned no row".to_string())?;
        Ok((
            SavingsTotal {
                saved_tokens: row
                    .get::<i64>(0)
                    .map_err(|error| format!("failed to decode saved tokens: {error}"))?
                    .max(0) as u64,
                calls: row
                    .get::<i64>(1)
                    .map_err(|error| format!("failed to decode savings calls: {error}"))?
                    .max(0) as u64,
            },
            row.get::<i64>(2)
                .map_err(|error| format!("failed to decode savings watermark: {error}"))?
                .max(0),
        ))
    }

    /// Per-day savings-ledger aggregation, newest day first. `Ok(vec![])` is
    /// the truthful "no settled rows in range"; snapshot, query, and decode
    /// failures stay errors instead of an empty history.
    #[hotpath::skip]
    pub async fn savings_history(
        &self,
        project: Option<&str>,
        since: i64,
    ) -> Result<Vec<SavingsDay>, String> {
        let project =
            project.map(|path| RegisteredGlobalDb::canonical_project_key(Path::new(path)));
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin savings history snapshot: {error}"))?;
        let (mut sql, values) = savings_scope_query(
            "SELECT (ts / 86400) * 86400 AS day,
                    COALESCE(SUM(CASE
                        WHEN before_tokens > after_tokens
                        THEN before_tokens - after_tokens
                        ELSE 0 END), 0),
                    COUNT(*)
             FROM savings_ledger",
            project.as_deref(),
            since,
        );
        sql.push_str(" GROUP BY day ORDER BY day DESC");
        let mut rows = snapshot
            .query(&sql, values)
            .await
            .map_err(|error| format!("failed to query savings history: {error}"))?;
        let mut history = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read savings history row: {error}"))?
        {
            history.push(SavingsDay {
                day: row
                    .get::<i64>(0)
                    .map_err(|error| format!("failed to decode savings history day: {error}"))?,
                saved_tokens: row
                    .get::<i64>(1)
                    .map_err(|error| {
                        format!("failed to decode savings history saved tokens: {error}")
                    })?
                    .max(0) as u64,
                calls: row
                    .get::<i64>(2)
                    .map_err(|error| format!("failed to decode savings history calls: {error}"))?
                    .max(0) as u64,
            });
        }
        Ok(history)
    }
}

fn tokens_saved_query(project_path: Option<&str>) -> (String, Vec<Value>) {
    let mut clauses = Vec::new();
    let mut values = Vec::new();
    push_optional_analytics_filter(&mut clauses, &mut values, "path", project_path);
    let sql = if clauses.is_empty() {
        "SELECT COALESCE(SUM(tokens_saved), 0) FROM projects".to_string()
    } else {
        format!(
            "SELECT COALESCE(SUM(tokens_saved), 0) FROM projects WHERE {}",
            clauses.join(" AND ")
        )
    };
    (sql, values)
}

fn savings_scope_query(select: &str, project_id: Option<&str>, since: i64) -> (String, Vec<Value>) {
    let mut clauses = Vec::new();
    let mut values = Vec::new();
    push_optional_analytics_filter(&mut clauses, &mut values, "project_path", project_id);
    values.push(Value::Integer(since));
    clauses.push(format!("ts >= ?{}", values.len()));
    (format!("{select} WHERE {}", clauses.join(" AND ")), values)
}
