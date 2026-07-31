use std::path::Path;

use super::{RegisteredGlobalDb, SavingsDay, SavingsTotal, global_db_operation_error};

impl RegisteredGlobalDb {
    pub(crate) async fn upsert(&self, project_path: &Path, tokens_saved: u64) {
        if let Err(error) = self
            .try_upsert_project_tokens(project_path, tokens_saved)
            .await
        {
            self.report_optional_accounting_failure("update project token total", &error);
        }
    }

    pub(crate) async fn try_upsert_project_tokens(
        &self,
        project_path: &Path,
        tokens_saved: u64,
    ) -> crate::errors::Result<()> {
        let path = super::project_path_alias_key(project_path);
        let transaction = self.begin_write_transaction().await?;
        transaction
            .execute(
                "INSERT INTO projects (path, tokens_saved) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET
                    tokens_saved = MAX(tokens_saved, excluded.tokens_saved)",
                crate::db::engine::params![path, tokens_saved as i64],
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
    pub(crate) async fn try_get_project_tokens(&self, project_path: &Path) -> Result<u64, String> {
        let path = super::project_path_alias_key(project_path);
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to open accounting snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT tokens_saved FROM projects WHERE path = ?1",
                crate::db::engine::params![path],
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

    pub(crate) async fn get_project_tokens(&self, project_path: &Path) -> Option<u64> {
        self.try_get_project_tokens(project_path).await.ok()
    }

    pub(crate) async fn try_global_tokens_saved(&self) -> Result<u64, String> {
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

    pub(crate) async fn global_tokens_saved(&self) -> Option<u64> {
        self.try_global_tokens_saved().await.ok()
    }

    pub(crate) async fn record_savings(
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

    pub(crate) async fn try_record_savings(
        &self,
        project_path: &str,
        tool_name: &str,
        before_tokens: u64,
        after_tokens: u64,
        timestamp: i64,
    ) -> crate::errors::Result<()> {
        let project_path = RegisteredGlobalDb::canonical_project_key(Path::new(project_path));
        let transaction = self.begin_write_transaction().await?;
        transaction
            .execute(
                "INSERT INTO savings_ledger
                     (ts, project_path, tool_name, before_tokens, after_tokens)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                crate::db::engine::params![
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
        error: &crate::errors::TraceDecayError,
    ) {
        tracing::error!(
            database = %self.db_path().display(),
            operation,
            error = %error,
            "optional global database accounting write failed"
        );
    }

    pub(crate) async fn sum_savings(&self, project: Option<&str>, since: i64) -> SavingsTotal {
        let project =
            project.map(|path| RegisteredGlobalDb::canonical_project_key(Path::new(path)));
        self.sum_savings_by_project_id(project.as_deref(), since)
            .await
    }

    /// Same aggregation for an already-resolved canonical project identity.
    /// Application read models use this to avoid reinterpreting identity as a path.
    pub(crate) async fn sum_savings_by_project_id(
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
    pub(crate) async fn sum_savings_by_project_id_checked(
        &self,
        project_id: Option<&str>,
        since: i64,
    ) -> Result<SavingsTotal, String> {
        self.savings_totals_with_watermark(project_id, since)
            .await
            .map(|(totals, _)| totals)
    }

    pub(crate) async fn savings_totals_with_watermark(
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
                        crate::db::engine::params![project, since],
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
                        crate::db::engine::params![since],
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

    pub(crate) async fn savings_history(
        &self,
        project: Option<&str>,
        since: i64,
    ) -> Vec<SavingsDay> {
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
                        crate::db::engine::params![project, since],
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
                        crate::db::engine::params![since],
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

    pub(crate) async fn insert_turn(&self, turn: &crate::types::CostTurn) -> bool {
        let Ok(transaction) = self.begin_write_transaction().await else {
            return false;
        };
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO turns
                     (message_id, project_hash, session_id, model, timestamp,
                      input_tokens, output_tokens, cache_write_tokens, cache_read_tokens,
                      cost_usd, category, tool_names)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                turn_params(turn),
            )
            .await
            .is_ok_and(|rows| rows > 0);
        if inserted {
            transaction.commit().await.is_ok()
        } else {
            false
        }
    }

    pub(crate) async fn insert_turns(&self, turns: &[crate::types::CostTurn]) -> usize {
        if turns.is_empty() {
            return 0;
        }
        let Ok(transaction) = self.begin_write_transaction().await else {
            return 0;
        };
        let mut inserted = 0;
        for turn in turns {
            match transaction
                .execute(
                    "INSERT OR IGNORE INTO turns
                         (message_id, project_hash, session_id, model, timestamp,
                          input_tokens, output_tokens, cache_write_tokens, cache_read_tokens,
                          cost_usd, category, tool_names)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    turn_params(turn),
                )
                .await
            {
                Ok(rows) => inserted += rows as usize,
                Err(_) => return 0,
            }
        }
        if transaction.commit().await.is_ok() {
            inserted
        } else {
            0
        }
    }

    /// Atomically imports accounting turns and advances the source cursor.
    ///
    /// A cursor failure rolls back every turn from the same scanned frontier,
    /// so retry cannot duplicate a partially acknowledged file segment.
    pub(crate) async fn insert_turns_with_cursor(
        &self,
        turns: &[crate::types::CostTurn],
        cursor_path: &str,
        cursor: super::ParseOffset,
    ) -> Result<(usize, f64, u64), String> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin accounting import transaction: {error}"))?;
        let mut inserted = 0usize;
        let mut cost_usd = 0.0;
        let mut tokens = 0u64;
        for turn in turns {
            let rows = transaction
                .execute(
                    "INSERT OR IGNORE INTO turns
                         (message_id, project_hash, session_id, model, timestamp,
                          input_tokens, output_tokens, cache_write_tokens, cache_read_tokens,
                          cost_usd, category, tool_names)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    turn_params(turn),
                )
                .await
                .map_err(|error| format!("failed to append accounting turn: {error}"))?;
            if rows > 0 {
                inserted = inserted.saturating_add(rows as usize);
                cost_usd += turn.cost_usd;
                tokens = tokens.saturating_add(turn.input_tokens + turn.output_tokens);
            }
        }
        super::transcript::set_parse_offset(&transaction, cursor_path, cursor)
            .await
            .map_err(|error| format!("failed to persist accounting import cursor: {error}"))?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit accounting import transaction: {error}"))?;
        Ok((inserted, cost_usd, tokens))
    }

    pub(crate) async fn try_total_cost_since(&self, since: u64) -> Result<f64, String> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to open accounting snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM turns WHERE timestamp >= ?1",
                crate::db::engine::params![since as i64],
            )
            .await
            .map_err(|error| format!("failed to query total cost: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read total cost row: {error}"))?
            .ok_or_else(|| "total cost query returned no row".to_string())?;
        row.get::<f64>(0)
            .map_err(|error| format!("failed to decode total cost: {error}"))
    }

    pub(crate) async fn try_total_tokens_since(&self, since: u64) -> Result<u64, String> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to open accounting snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT COALESCE(SUM(input_tokens + output_tokens), 0)
                 FROM turns WHERE timestamp >= ?1",
                crate::db::engine::params![since as i64],
            )
            .await
            .map_err(|error| format!("failed to query total tokens: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read total tokens row: {error}"))?
            .ok_or_else(|| "total tokens query returned no row".to_string())?;
        let total = row
            .get::<i64>(0)
            .map_err(|error| format!("failed to decode total tokens: {error}"))?;
        u64::try_from(total).map_err(|_| format!("total tokens cannot be negative: {total}"))
    }

    /// One-snapshot denominator and aggregate for the canonical turn store.
    pub(crate) async fn accounting_totals_since(&self, since: u64) -> Option<(u64, u64, f64, i64)> {
        let snapshot = self.read_snapshot().await.ok()?;
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*),
                        COALESCE(SUM(input_tokens + output_tokens), 0),
                        COALESCE(SUM(cost_usd), 0.0),
                        COALESCE(MAX(timestamp), 0)
                 FROM turns WHERE timestamp >= ?1",
                crate::db::engine::params![since as i64],
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        Some((
            row.get::<i64>(0).ok()?.max(0) as u64,
            row.get::<i64>(1).ok()?.max(0) as u64,
            row.get::<f64>(2).ok()?,
            row.get::<i64>(3).ok()?.max(0),
        ))
    }

    pub(crate) async fn try_token_breakdown_since(
        &self,
        since: u64,
    ) -> Result<(u64, u64, u64), String> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to open accounting snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT COALESCE(SUM(input_tokens), 0),
                        COALESCE(SUM(output_tokens), 0),
                        COALESCE(SUM(cache_read_tokens), 0)
                 FROM turns WHERE timestamp >= ?1",
                crate::db::engine::params![since as i64],
            )
            .await
            .map_err(|error| format!("failed to query token breakdown: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read token breakdown row: {error}"))?
            .ok_or_else(|| "token breakdown query returned no row".to_string())?;
        let read = |index| {
            let value = row
                .get::<i64>(index)
                .map_err(|error| format!("failed to decode token breakdown: {error}"))?;
            u64::try_from(value)
                .map_err(|_| format!("token breakdown cannot contain a negative value: {value}"))
        };
        Ok((read(0)?, read(1)?, read(2)?))
    }

    pub(crate) async fn try_cost_by_model_since(
        &self,
        since: u64,
    ) -> Result<Vec<(String, f64, u64)>, String> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to open accounting snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT model, SUM(cost_usd), SUM(input_tokens + output_tokens)
                 FROM turns WHERE timestamp >= ?1
                 GROUP BY model ORDER BY SUM(cost_usd) DESC",
                crate::db::engine::params![since as i64],
            )
            .await
            .map_err(|error| format!("failed to query model cost breakdown: {error}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read model cost breakdown row: {error}"))?
        {
            let tokens = row
                .get::<i64>(2)
                .map_err(|error| format!("failed to decode model token total: {error}"))?;
            out.push((
                row.get::<String>(0)
                    .map_err(|error| format!("failed to decode model name: {error}"))?,
                row.get::<f64>(1)
                    .map_err(|error| format!("failed to decode model cost: {error}"))?,
                u64::try_from(tokens)
                    .map_err(|_| format!("model token total cannot be negative: {tokens}"))?,
            ));
        }
        Ok(out)
    }

    pub(crate) async fn cost_by_model_since(&self, since: u64) -> Vec<(String, f64, u64)> {
        self.try_cost_by_model_since(since)
            .await
            .unwrap_or_default()
    }

    pub(crate) async fn try_cost_by_category_since(
        &self,
        since: u64,
    ) -> Result<Vec<(String, f64, u64)>, String> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to open accounting snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT category, SUM(cost_usd), COUNT(*)
                 FROM turns WHERE timestamp >= ?1
                 GROUP BY category ORDER BY SUM(cost_usd) DESC",
                crate::db::engine::params![since as i64],
            )
            .await
            .map_err(|error| format!("failed to query category cost breakdown: {error}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read category cost breakdown row: {error}"))?
        {
            let turns = row
                .get::<i64>(2)
                .map_err(|error| format!("failed to decode category turn count: {error}"))?;
            out.push((
                row.get::<String>(0)
                    .map_err(|error| format!("failed to decode category name: {error}"))?,
                row.get::<f64>(1)
                    .map_err(|error| format!("failed to decode category cost: {error}"))?,
                u64::try_from(turns)
                    .map_err(|_| format!("category turn count cannot be negative: {turns}"))?,
            ));
        }
        Ok(out)
    }

}

fn turn_params(turn: &crate::types::CostTurn) -> crate::db::engine::Params {
    crate::db::engine::params![
        turn.message_id.as_str(),
        turn.project_hash.as_str(),
        turn.session_id.as_str(),
        turn.model.as_str(),
        turn.timestamp as i64,
        turn.input_tokens as i64,
        turn.output_tokens as i64,
        turn.cache_write_tokens as i64,
        turn.cache_read_tokens as i64,
        turn.cost_usd,
        turn.category.as_str(),
        turn.tool_names.as_str(),
    ]
}
