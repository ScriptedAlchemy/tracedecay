use std::path::Path;

use super::{RegisteredGlobalDb, SavingsDay, SavingsTotal, global_db_operation_error};

impl RegisteredGlobalDb {
    pub async fn upsert(&self, project_path: &Path, tokens_saved: u64) {
        if let Err(error) = self
            .try_upsert_project_tokens(project_path, tokens_saved)
            .await
        {
            self.report_optional_accounting_failure("update project token total", &error);
        }
    }

    pub async fn try_upsert_project_tokens(
        &self,
        project_path: &Path,
        tokens_saved: u64,
    ) -> tracedecay_runtime_core::errors::Result<()> {
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
    pub async fn try_get_project_tokens(&self, project_path: &Path) -> Result<u64, String> {
        let path = super::project_path_alias_key(project_path);
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to open accounting snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT tokens_saved FROM projects WHERE path = ?1",
                tracedecay_runtime_core::db::engine::params![path],
            )
            .await
            .map_err(|error| format!("failed to query project tokens saved: {error}"))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read project tokens saved row: {error}"))?
        else {
            return Ok(0);
        };
        let total = row
            .get::<i64>(0)
            .map_err(|error| format!("failed to decode project tokens saved: {error}"))?;
        u64::try_from(total)
            .map_err(|_| format!("project tokens saved cannot be negative: {total}"))
    }

    pub async fn get_project_tokens(&self, project_path: &Path) -> Option<u64> {
        self.try_get_project_tokens(project_path).await.ok()
    }

    pub async fn try_global_tokens_saved(&self) -> Result<u64, String> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to open accounting snapshot: {error}"))?;
        let mut rows = snapshot
            .query("SELECT COALESCE(SUM(tokens_saved), 0) FROM projects", ())
            .await
            .map_err(|error| format!("failed to query global tokens saved: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read global tokens saved row: {error}"))?
            .ok_or_else(|| "global tokens saved query returned no row".to_string())?;
        let total = row
            .get::<i64>(0)
            .map_err(|error| format!("failed to decode global tokens saved: {error}"))?;
        u64::try_from(total).map_err(|_| format!("global tokens saved cannot be negative: {total}"))
    }

    pub async fn global_tokens_saved(&self) -> Option<u64> {
        self.try_global_tokens_saved().await.ok()
    }

    pub async fn record_savings(
        &self,
        project_path: &str,
        tool_name: &str,
        before_tokens: u64,
        after_tokens: u64,
        timestamp: i64,
    ) {
        if let Err(error) = self
            .try_record_savings(
                project_path,
                tool_name,
                before_tokens,
                after_tokens,
                timestamp,
            )
            .await
        {
            self.report_optional_accounting_failure("append savings ledger entry", &error);
        }
    }

    pub async fn try_record_savings(
        &self,
        project_path: &str,
        tool_name: &str,
        before_tokens: u64,
        after_tokens: u64,
        timestamp: i64,
    ) -> tracedecay_runtime_core::errors::Result<()> {
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

    fn report_optional_accounting_failure(
        &self,
        operation: &'static str,
        error: &tracedecay_runtime_core::errors::TraceDecayError,
    ) {
        tracing::error!(
            database = %self.db_path().display(),
            operation,
            error = %error,
            "optional global database accounting write failed"
        );
    }

    pub async fn sum_savings(&self, project: Option<&str>, since: i64) -> SavingsTotal {
        let project =
            project.map(|path| RegisteredGlobalDb::canonical_project_key(Path::new(path)));
        self.sum_savings_by_project_id(project.as_deref(), since)
            .await
    }

    /// Same aggregation for an already-resolved canonical project identity.
    /// Application read models use this to avoid reinterpreting identity as a path.
    pub async fn sum_savings_by_project_id(
        &self,
        project_id: Option<&str>,
        since: i64,
    ) -> SavingsTotal {
        self.sum_savings_by_project_id_checked(project_id, since)
            .await
            .unwrap_or(SavingsTotal {
                saved_tokens: 0,
                calls: 0,
            })
    }

    /// Checked form used by denominator-safe read models. A failed read must
    /// remain unavailable instead of becoming a trustworthy zero.
    pub async fn sum_savings_by_project_id_checked(
        &self,
        project_id: Option<&str>,
        since: i64,
    ) -> Result<SavingsTotal, String> {
        self.savings_totals_with_watermark(project_id, since)
            .await
            .map(|(totals, _)| totals)
    }

    pub async fn savings_totals_with_watermark(
        &self,
        project_id: Option<&str>,
        since: i64,
    ) -> Result<(SavingsTotal, i64), String> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin savings snapshot: {error}"))?;
        let rows = match project_id {
            Some(project) => {
                snapshot
                    .query(
                        "SELECT COALESCE(SUM(CASE
                                WHEN before_tokens > after_tokens
                                THEN before_tokens - after_tokens
                                ELSE 0 END), 0),
                                COUNT(*),
                                COALESCE(MAX(id), 0)
                         FROM savings_ledger
                         WHERE project_path = ?1 AND ts >= ?2",
                        tracedecay_runtime_core::db::engine::params![project, since],
                    )
                    .await
            }
            None => {
                snapshot
                    .query(
                        "SELECT COALESCE(SUM(CASE
                                WHEN before_tokens > after_tokens
                                THEN before_tokens - after_tokens
                                ELSE 0 END), 0),
                                COUNT(*),
                                COALESCE(MAX(id), 0)
                         FROM savings_ledger
                         WHERE ts >= ?1",
                        tracedecay_runtime_core::db::engine::params![since],
                    )
                    .await
            }
        };
        let mut rows = rows.map_err(|error| format!("failed to query savings totals: {error}"))?;
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

    pub async fn savings_history(&self, project: Option<&str>, since: i64) -> Vec<SavingsDay> {
        let project =
            project.map(|path| RegisteredGlobalDb::canonical_project_key(Path::new(path)));
        let Ok(snapshot) = self.read_snapshot().await else {
            return Vec::new();
        };
        let rows = match project.as_deref() {
            Some(project) => {
                snapshot
                    .query(
                        "SELECT (ts / 86400) * 86400 AS day,
                                COALESCE(SUM(CASE
                                    WHEN before_tokens > after_tokens
                                    THEN before_tokens - after_tokens
                                    ELSE 0 END), 0),
                                COUNT(*)
                         FROM savings_ledger
                         WHERE project_path = ?1 AND ts >= ?2
                         GROUP BY day ORDER BY day DESC",
                        tracedecay_runtime_core::db::engine::params![project, since],
                    )
                    .await
            }
            None => {
                snapshot
                    .query(
                        "SELECT (ts / 86400) * 86400 AS day,
                                COALESCE(SUM(CASE
                                    WHEN before_tokens > after_tokens
                                    THEN before_tokens - after_tokens
                                    ELSE 0 END), 0),
                                COUNT(*)
                         FROM savings_ledger
                         WHERE ts >= ?1
                         GROUP BY day ORDER BY day DESC",
                        tracedecay_runtime_core::db::engine::params![since],
                    )
                    .await
            }
        };
        let Ok(mut rows) = rows else {
            return Vec::new();
        };
        let mut history = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            history.push(SavingsDay {
                day: row.get::<i64>(0).unwrap_or(0),
                saved_tokens: row.get::<i64>(1).unwrap_or(0).max(0) as u64,
                calls: row.get::<i64>(2).unwrap_or(0).max(0) as u64,
            });
        }
        history
    }
}
