//! User-level database that tracks all `TokenSave` projects and their saved tokens.
//!
//! Stored at `~/.tokensave/global.db`, this DB holds one row per project with
//! the project's DB path and its cumulative tokens-saved count. All operations
//! are best-effort: failures are silently ignored so they never block the main
//! MCP server loop.

use std::path::{Path, PathBuf};

use libsql::{params, Builder, Connection, Database as LibsqlDatabase, Value};

use crate::sessions::{
    SessionMessageRecord, SessionMessageSearchResult, SessionRecord, SessionSearchScope,
};

const MAX_SESSION_MESSAGE_TEXT_BYTES: usize = 256 * 1024;
const SESSION_MESSAGE_TRUNCATION_MARKER: &str = "\n[truncated by tokensave]";

/// Total savings + call count for a project (or all projects when `project` is None).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SavingsTotal {
    pub saved_tokens: u64,
    pub calls: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SavingsDay {
    /// Start-of-day epoch seconds (UTC).
    pub day: i64,
    pub saved_tokens: u64,
    pub calls: u64,
}

/// User-level database tracking all `TokenSave` projects.
pub struct GlobalDb {
    conn: Connection,
    _db: LibsqlDatabase,
}

const GLOBAL_DB_PATH_ENV: &str = "TOKENSAVE_GLOBAL_DB";

/// Returns the path to the global database: `~/.tokensave/global.db`.
pub fn global_db_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(GLOBAL_DB_PATH_ENV).filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    dirs::home_dir().map(|h| h.join(".tokensave").join("global.db"))
}

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |s| Value::Text(s.to_string()))
}

fn capped_session_message_text(text: &str) -> std::borrow::Cow<'_, str> {
    if text.len() <= MAX_SESSION_MESSAGE_TEXT_BYTES {
        return std::borrow::Cow::Borrowed(text);
    }

    let budget =
        MAX_SESSION_MESSAGE_TEXT_BYTES.saturating_sub(SESSION_MESSAGE_TRUNCATION_MARKER.len());
    let mut end = 0;
    for (idx, ch) in text.char_indices() {
        let next = idx + ch.len_utf8();
        if next > budget {
            break;
        }
        end = next;
    }

    let mut capped = String::with_capacity(end + SESSION_MESSAGE_TRUNCATION_MARKER.len());
    capped.push_str(&text[..end]);
    capped.push_str(SESSION_MESSAGE_TRUNCATION_MARKER);
    std::borrow::Cow::Owned(capped)
}

fn opt_i64(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}

fn row_to_session(row: &libsql::Row) -> Option<SessionRecord> {
    Some(SessionRecord {
        provider: row.get(0).ok()?,
        session_id: row.get(1).ok()?,
        project_key: row.get(2).ok()?,
        project_path: row.get(3).ok()?,
        title: row.get(4).ok()?,
        started_at: row.get(5).ok()?,
        ended_at: row.get(6).ok()?,
        transcript_path: row.get(7).ok()?,
        metadata_json: row.get(8).ok()?,
        parent_session_id: row.get(9).ok()?,
        is_subagent: row.get::<i64>(10).unwrap_or(0) != 0,
        agent_id: row.get(11).ok()?,
        parent_tool_use_id: row.get(12).ok()?,
    })
}

fn row_to_message(row: &libsql::Row, offset: i32) -> Option<SessionMessageRecord> {
    Some(SessionMessageRecord {
        provider: row.get(offset).ok()?,
        message_id: row.get(offset + 1).ok()?,
        session_id: row.get(offset + 2).ok()?,
        role: row.get(offset + 3).ok()?,
        timestamp: row.get(offset + 4).ok()?,
        ordinal: row.get(offset + 5).ok()?,
        text: row.get(offset + 6).ok()?,
        kind: row.get(offset + 7).ok()?,
        model: row.get(offset + 8).ok()?,
        tool_names: row.get(offset + 9).ok()?,
        source_path: row.get(offset + 10).ok()?,
        source_offset: row.get(offset + 11).ok()?,
        metadata_json: row.get(offset + 12).ok()?,
    })
}

fn session_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter_map(|word| {
            let sanitized: String = word.chars().filter(|c| *c != '"').collect();
            if sanitized.is_empty() {
                None
            } else {
                Some(format!("\"{sanitized}\"*"))
            }
        })
        .collect::<Vec<_>>()
        .join(" OR ")
}

async fn session_column_exists(conn: &Connection, column: &str) -> bool {
    let Ok(mut rows) = conn.query("PRAGMA table_info(sessions)", ()).await else {
        return false;
    };
    while let Ok(Some(row)) = rows.next().await {
        if row.get::<String>(1).ok().as_deref() == Some(column) {
            return true;
        }
    }
    false
}

async fn ensure_session_parent_columns(conn: &Connection) -> Option<()> {
    for (column, ddl) in [
        (
            "parent_session_id",
            "ALTER TABLE sessions ADD COLUMN parent_session_id TEXT",
        ),
        (
            "is_subagent",
            "ALTER TABLE sessions ADD COLUMN is_subagent INTEGER NOT NULL DEFAULT 0",
        ),
        ("agent_id", "ALTER TABLE sessions ADD COLUMN agent_id TEXT"),
        (
            "parent_tool_use_id",
            "ALTER TABLE sessions ADD COLUMN parent_tool_use_id TEXT",
        ),
    ] {
        if !session_column_exists(conn, column).await {
            add_session_parent_column_after_missing_check(conn, column, ddl).await?;
        }
    }
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_sessions_parent
            ON sessions(provider, parent_session_id)",
        (),
    )
    .await
    .ok()?;
    Some(())
}

async fn add_session_parent_column_after_missing_check(
    conn: &Connection,
    column: &str,
    ddl: &str,
) -> Option<()> {
    match conn.execute(ddl, ()).await {
        Ok(_) => Some(()),
        Err(_) if session_column_exists(conn, column).await => Some(()),
        Err(_) => None,
    }
}

impl GlobalDb {
    /// Opens (or creates) the global database at an explicit path. Returns
    /// `None` if the directory cannot be created or the DB fails to open.
    pub async fn open_at(db_path: &std::path::Path) -> Option<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }

        let db = Builder::new_local(db_path).build().await.ok()?;
        let conn = db.connect().ok()?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = NORMAL;",
        )
        .await
        .ok()?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (
                path TEXT PRIMARY KEY,
                tokens_saved INTEGER NOT NULL DEFAULT 0
            )",
        )
        .await
        .ok()?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS turns (
                message_id TEXT PRIMARY KEY,
                project_hash TEXT NOT NULL,
                session_id TEXT NOT NULL,
                model TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd REAL NOT NULL,
                category TEXT NOT NULL,
                tool_names TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_turns_timestamp ON turns(timestamp);
            CREATE INDEX IF NOT EXISTS idx_turns_project ON turns(project_hash);
            CREATE INDEX IF NOT EXISTS idx_turns_model ON turns(model);
            CREATE TABLE IF NOT EXISTS parse_offsets (
                file_path TEXT PRIMARY KEY,
                byte_offset INTEGER NOT NULL,
                mtime INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS savings_ledger (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                project_path TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                before_tokens INTEGER NOT NULL,
                after_tokens INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_savings_ledger_ts ON savings_ledger(ts);
            CREATE INDEX IF NOT EXISTS idx_savings_ledger_project ON savings_ledger(project_path);
            CREATE TABLE IF NOT EXISTS sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                project_path TEXT NOT NULL,
                title TEXT,
                started_at INTEGER,
                ended_at INTEGER,
                transcript_path TEXT,
                metadata_json TEXT,
                parent_session_id TEXT,
                is_subagent INTEGER NOT NULL DEFAULT 0,
                agent_id TEXT,
                parent_tool_use_id TEXT,
                PRIMARY KEY(provider, session_id)
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_project
                ON sessions(provider, project_key);
            CREATE INDEX IF NOT EXISTS idx_sessions_started_at
                ON sessions(started_at);
            CREATE TABLE IF NOT EXISTS session_messages (
                provider TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                timestamp INTEGER,
                ordinal INTEGER NOT NULL,
                text TEXT NOT NULL,
                kind TEXT,
                model TEXT,
                tool_names TEXT,
                source_path TEXT,
                source_offset INTEGER,
                metadata_json TEXT,
                PRIMARY KEY(provider, message_id),
                FOREIGN KEY(provider, session_id)
                    REFERENCES sessions(provider, session_id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_session_messages_session
                ON session_messages(provider, session_id, ordinal);
            CREATE INDEX IF NOT EXISTS idx_session_messages_timestamp
                ON session_messages(timestamp);
            CREATE INDEX IF NOT EXISTS idx_session_messages_source
                ON session_messages(source_path);
            CREATE VIRTUAL TABLE IF NOT EXISTS session_messages_fts USING fts5(
                text, role, kind, model, tool_names,
                content='session_messages', content_rowid='rowid'
            );
            CREATE TRIGGER IF NOT EXISTS session_messages_fts_insert
                AFTER INSERT ON session_messages BEGIN
                    INSERT INTO session_messages_fts(rowid, text, role, kind, model, tool_names)
                    VALUES (NEW.rowid, NEW.text, NEW.role, NEW.kind, NEW.model, NEW.tool_names);
                END;
            CREATE TRIGGER IF NOT EXISTS session_messages_fts_delete
                AFTER DELETE ON session_messages BEGIN
                    INSERT INTO session_messages_fts(session_messages_fts, rowid, text, role, kind, model, tool_names)
                    VALUES ('delete', OLD.rowid, OLD.text, OLD.role, OLD.kind, OLD.model, OLD.tool_names);
                END;
            CREATE TRIGGER IF NOT EXISTS session_messages_fts_update
                AFTER UPDATE ON session_messages BEGIN
                    INSERT INTO session_messages_fts(session_messages_fts, rowid, text, role, kind, model, tool_names)
                    VALUES ('delete', OLD.rowid, OLD.text, OLD.role, OLD.kind, OLD.model, OLD.tool_names);
                    INSERT INTO session_messages_fts(rowid, text, role, kind, model, tool_names)
                    VALUES (NEW.rowid, NEW.text, NEW.role, NEW.kind, NEW.model, NEW.tool_names);
                END",
        )
        .await
        .ok()?;
        ensure_session_parent_columns(&conn).await?;

        Some(Self { conn, _db: db })
    }

    /// Opens (or creates) the global database. Returns `None` if the home
    /// directory cannot be determined or the DB fails to open.
    pub async fn open() -> Option<Self> {
        let db_path = global_db_path()?;
        Self::open_at(&db_path).await
    }

    /// Registers or updates a project's tokens-saved count. Best-effort.
    pub async fn upsert(&self, project_path: &Path, tokens_saved: u64) {
        let path_str = project_path.to_string_lossy().to_string();
        let _ = self
            .conn
            .execute(
                "INSERT INTO projects (path, tokens_saved) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET tokens_saved = ?2",
                params![path_str, tokens_saved as i64],
            )
            .await;
    }

    /// Returns the stored `tokens_saved` count for a specific project, or 0 if not found.
    pub async fn get_project_tokens(&self, project_path: &Path) -> u64 {
        let path_str = project_path.to_string_lossy().to_string();
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT tokens_saved FROM projects WHERE path = ?1",
                params![path_str],
            )
            .await
        else {
            return 0;
        };
        match rows.next().await {
            Ok(Some(row)) => row.get::<i64>(0).unwrap_or(0) as u64,
            _ => 0,
        }
    }

    /// Returns the sum of `tokens_saved` across all tracked projects.
    pub async fn global_tokens_saved(&self) -> Option<u64> {
        let mut rows = self
            .conn
            .query("SELECT COALESCE(SUM(tokens_saved), 0) FROM projects", ())
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        let total: i64 = row.get(0).ok()?;
        Some(total as u64)
    }

    /// Insert a new ledger row. Best-effort; errors are reported to stderr via eprintln
    /// but never propagated.
    pub async fn record_savings(
        &self,
        project_path: &str,
        tool_name: &str,
        before_tokens: u64,
        after_tokens: u64,
        ts: i64,
    ) {
        let result = self
            .conn
            .execute(
                "INSERT INTO savings_ledger (ts, project_path, tool_name, before_tokens, after_tokens) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![ts, project_path, tool_name, before_tokens as i64, after_tokens as i64],
            )
            .await;
        if let Err(e) = result {
            eprintln!("[tokensave] savings_ledger insert failed: {e}");
        }
    }

    /// Sum (before-after) across the ledger entries, with `ts >= since`. Optionally
    /// filter by exact project path. Returns zeros on any DB error.
    pub async fn sum_savings(&self, project: Option<&str>, since: i64) -> SavingsTotal {
        let sql_with_project =
            "SELECT COALESCE(SUM(CASE WHEN before_tokens > after_tokens THEN before_tokens - after_tokens ELSE 0 END), 0), COUNT(*) \
             FROM savings_ledger WHERE project_path = ?1 AND ts >= ?2";
        let sql_all =
            "SELECT COALESCE(SUM(CASE WHEN before_tokens > after_tokens THEN before_tokens - after_tokens ELSE 0 END), 0), COUNT(*) \
             FROM savings_ledger WHERE ts >= ?1";

        let rows = match project {
            Some(p) => self.conn.query(sql_with_project, params![p, since]).await,
            None => self.conn.query(sql_all, params![since]).await,
        };
        let Ok(mut rows) = rows else {
            return SavingsTotal {
                saved_tokens: 0,
                calls: 0,
            };
        };
        match rows.next().await {
            Ok(Some(row)) => SavingsTotal {
                saved_tokens: row.get::<i64>(0).unwrap_or(0).max(0) as u64,
                calls: row.get::<i64>(1).unwrap_or(0).max(0) as u64,
            },
            _ => SavingsTotal {
                saved_tokens: 0,
                calls: 0,
            },
        }
    }

    /// Group ledger entries by UTC calendar day. Newest-first.
    pub async fn savings_history(&self, project: Option<&str>, since: i64) -> Vec<SavingsDay> {
        let sql_with_project =
            "SELECT (ts/86400)*86400 AS day, \
                    COALESCE(SUM(CASE WHEN before_tokens > after_tokens THEN before_tokens - after_tokens ELSE 0 END), 0), \
                    COUNT(*) \
             FROM savings_ledger WHERE project_path = ?1 AND ts >= ?2 \
             GROUP BY day ORDER BY day DESC";
        let sql_all =
            "SELECT (ts/86400)*86400 AS day, \
                    COALESCE(SUM(CASE WHEN before_tokens > after_tokens THEN before_tokens - after_tokens ELSE 0 END), 0), \
                    COUNT(*) \
             FROM savings_ledger WHERE ts >= ?1 \
             GROUP BY day ORDER BY day DESC";

        let rows = match project {
            Some(p) => self.conn.query(sql_with_project, params![p, since]).await,
            None => self.conn.query(sql_all, params![since]).await,
        };
        let Ok(mut rows) = rows else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            out.push(SavingsDay {
                day: row.get::<i64>(0).unwrap_or(0),
                saved_tokens: row.get::<i64>(1).unwrap_or(0).max(0) as u64,
                calls: row.get::<i64>(2).unwrap_or(0).max(0) as u64,
            });
        }
        out
    }

    /// Removes a project's row from the global DB. Best-effort.
    pub async fn delete_project(&self, project_path: &Path) {
        let path_str = project_path.to_string_lossy().to_string();
        let _ = self
            .conn
            .execute("DELETE FROM projects WHERE path = ?1", params![path_str])
            .await;
    }

    /// Removes many project rows in a single statement. Returns the number of
    /// rows actually deleted (0 on any error). Best-effort.
    ///
    /// Chunks the input at 256 paths per statement to stay well clear of
    /// `SQLite`'s default 999-parameter limit while still reducing N round trips
    /// to ⌈N/256⌉.
    pub async fn delete_projects(&self, project_paths: &[String]) -> usize {
        const CHUNK: usize = 256;
        let mut total: usize = 0;
        for chunk in project_paths.chunks(CHUNK) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders: Vec<&str> = chunk.iter().map(|_| "?").collect();
            let sql = format!(
                "DELETE FROM projects WHERE path IN ({})",
                placeholders.join(",")
            );
            let values: Vec<libsql::Value> = chunk
                .iter()
                .map(|p| libsql::Value::Text(p.clone()))
                .collect();
            if let Ok(n) = self.conn.execute(&sql, values).await {
                total = total.saturating_add(n as usize);
            }
        }
        total
    }

    /// Returns all tracked project paths.
    pub async fn list_project_paths(&self) -> Vec<String> {
        let Ok(mut rows) = self.conn.query("SELECT path FROM projects", ()).await else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            if let Ok(path) = row.get::<String>(0) {
                paths.push(path);
            }
        }
        paths
    }

    /// Inserts or replaces a provider session. Returns `false` on any DB error.
    pub async fn upsert_session(&self, session: &SessionRecord) -> bool {
        self.conn
            .execute(
                "INSERT INTO sessions
                 (provider, session_id, project_key, project_path, title, started_at, ended_at,
                  transcript_path, metadata_json, parent_session_id, is_subagent, agent_id,
                  parent_tool_use_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(provider, session_id) DO UPDATE SET
                    project_key = excluded.project_key,
                    project_path = excluded.project_path,
                    title = excluded.title,
                    started_at = excluded.started_at,
                    ended_at = excluded.ended_at,
                    transcript_path = excluded.transcript_path,
                    metadata_json = excluded.metadata_json,
                    parent_session_id = excluded.parent_session_id,
                    is_subagent = excluded.is_subagent,
                    agent_id = excluded.agent_id,
                    parent_tool_use_id = excluded.parent_tool_use_id",
                params![
                    session.provider.as_str(),
                    session.session_id.as_str(),
                    session.project_key.as_str(),
                    session.project_path.as_str(),
                    opt_text(session.title.as_deref()),
                    opt_i64(session.started_at),
                    opt_i64(session.ended_at),
                    opt_text(session.transcript_path.as_deref()),
                    opt_text(session.metadata_json.as_deref()),
                    opt_text(session.parent_session_id.as_deref()),
                    if session.is_subagent { 1_i64 } else { 0_i64 },
                    opt_text(session.agent_id.as_deref()),
                    opt_text(session.parent_tool_use_id.as_deref()),
                ],
            )
            .await
            .is_ok()
    }

    /// Returns a single provider session by its provider-local ID.
    pub async fn get_session(&self, provider: &str, session_id: &str) -> Option<SessionRecord> {
        let mut rows = self
            .conn
            .query(
                "SELECT provider, session_id, project_key, project_path, title, started_at,
                        ended_at, transcript_path, metadata_json, parent_session_id,
                        is_subagent, agent_id, parent_tool_use_id
                 FROM sessions WHERE provider = ?1 AND session_id = ?2",
                params![provider, session_id],
            )
            .await
            .ok()?;
        row_to_session(&rows.next().await.ok()??)
    }

    /// Inserts or replaces a provider message. Returns `false` on any DB error.
    pub async fn upsert_session_message(&self, message: &SessionMessageRecord) -> bool {
        let text = capped_session_message_text(&message.text);
        self.conn
            .execute(
                "INSERT INTO session_messages
                 (provider, message_id, session_id, role, timestamp, ordinal, text, kind, model,
                  tool_names, source_path, source_offset, metadata_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(provider, message_id) DO UPDATE SET
                    session_id = excluded.session_id,
                    role = excluded.role,
                    timestamp = excluded.timestamp,
                    ordinal = excluded.ordinal,
                    text = excluded.text,
                    kind = excluded.kind,
                    model = excluded.model,
                    tool_names = excluded.tool_names,
                    source_path = excluded.source_path,
                    source_offset = excluded.source_offset,
                    metadata_json = excluded.metadata_json",
                params![
                    message.provider.as_str(),
                    message.message_id.as_str(),
                    message.session_id.as_str(),
                    message.role.as_str(),
                    opt_i64(message.timestamp),
                    message.ordinal,
                    text.as_ref(),
                    opt_text(message.kind.as_deref()),
                    opt_text(message.model.as_deref()),
                    opt_text(message.tool_names.as_deref()),
                    opt_text(message.source_path.as_deref()),
                    opt_i64(message.source_offset),
                    opt_text(message.metadata_json.as_deref()),
                ],
            )
            .await
            .is_ok()
    }

    /// Returns a single provider message by its provider-local ID.
    pub async fn get_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<SessionMessageRecord> {
        let mut rows = self
            .conn
            .query(
                "SELECT provider, message_id, session_id, role, timestamp, ordinal, text, kind,
                        model, tool_names, source_path, source_offset, metadata_json
                 FROM session_messages WHERE provider = ?1 AND message_id = ?2",
                params![provider, message_id],
            )
            .await
            .ok()?;
        row_to_message(&rows.next().await.ok()??, 0)
    }

    /// Searches message text for a provider, optionally constrained to one project.
    pub async fn search_session_messages(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Vec<SessionMessageSearchResult> {
        self.search_session_messages_filtered(
            provider,
            project_key,
            query,
            limit,
            SessionSearchScope::All,
            None,
        )
        .await
    }

    /// Searches message text with optional parent/subagent relationship filters.
    pub async fn search_session_messages_filtered(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        scope: SessionSearchScope,
        parent_session_id: Option<&str>,
    ) -> Vec<SessionMessageSearchResult> {
        let fts_query = session_fts_query(query);
        if fts_query.is_empty() || limit == 0 {
            return Vec::new();
        }

        let select = "SELECT
                s.provider, s.session_id, s.project_key, s.project_path, s.title, s.started_at,
                s.ended_at, s.transcript_path, s.metadata_json, s.parent_session_id,
                s.is_subagent, s.agent_id, s.parent_tool_use_id,
                m.provider, m.message_id, m.session_id, m.role, m.timestamp, m.ordinal, m.text,
                m.kind, m.model, m.tool_names, m.source_path, m.source_offset, m.metadata_json,
                bm25(session_messages_fts, 10.0, 2.0, 1.0, 1.0, 1.0) AS rank
             FROM session_messages_fts
             JOIN session_messages m ON session_messages_fts.rowid = m.rowid
             JOIN sessions s ON s.provider = m.provider AND s.session_id = m.session_id
             WHERE session_messages_fts MATCH ?1 AND m.provider = ?2
               AND (?3 IS NULL OR s.project_key = ?3)
               AND (?4 IS NULL OR s.parent_session_id = ?4)
               AND (?5 != 1 OR s.is_subagent = 0)
               AND (?6 != 1 OR s.is_subagent = 1)";
        let order = " ORDER BY bm25(session_messages_fts, 10.0, 2.0, 1.0, 1.0, 1.0)
                      LIMIT ?7";
        let parents_only = i64::from(matches!(scope, SessionSearchScope::ParentsOnly));
        let subagents_only = i64::from(matches!(scope, SessionSearchScope::SubagentsOnly));
        let rows_result = self
            .conn
            .query(
                &format!("{select}{order}"),
                params![
                    fts_query.as_str(),
                    provider,
                    opt_text(project_key),
                    opt_text(parent_session_id),
                    parents_only,
                    subagents_only,
                    limit as i64,
                ],
            )
            .await;

        let Ok(mut rows) = rows_result else {
            return Vec::new();
        };

        let mut results = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            let Some(session) = row_to_session(&row) else {
                continue;
            };
            let Some(message) = row_to_message(&row, 13) else {
                continue;
            };
            let score = row.get::<f64>(26).map_or(0.0, |rank| -rank);
            results.push(SessionMessageSearchResult {
                session,
                message,
                score,
            });
        }
        results
    }

    // ── Accounting: turns table ──────────────────────────────────────

    /// Insert a parsed turn. Returns `true` if inserted, `false` if duplicate.
    pub async fn insert_turn(&self, turn: &crate::types::CostTurn) -> bool {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO turns
                 (message_id, project_hash, session_id, model, timestamp,
                  input_tokens, output_tokens, cache_write_tokens, cache_read_tokens,
                  cost_usd, category, tool_names)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    turn.message_id.clone(),
                    turn.project_hash.clone(),
                    turn.session_id.clone(),
                    turn.model.clone(),
                    turn.timestamp as i64,
                    turn.input_tokens as i64,
                    turn.output_tokens as i64,
                    turn.cache_write_tokens as i64,
                    turn.cache_read_tokens as i64,
                    turn.cost_usd,
                    turn.category.clone(),
                    turn.tool_names.clone(),
                ],
            )
            .await
            .is_ok_and(|n| n > 0)
    }

    /// Total cost in USD since a given unix timestamp.
    pub async fn total_cost_since(&self, since: u64) -> Option<f64> {
        let mut rows = self
            .conn
            .query(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM turns WHERE timestamp >= ?1",
                params![since as i64],
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        Some(row.get::<f64>(0).unwrap_or(0.0))
    }

    /// Total input + output tokens since a given unix timestamp.
    pub async fn total_tokens_since(&self, since: u64) -> Option<u64> {
        let mut rows = self
            .conn
            .query(
                "SELECT COALESCE(SUM(input_tokens + output_tokens), 0) FROM turns WHERE timestamp >= ?1",
                params![since as i64],
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        Some(row.get::<i64>(0).unwrap_or(0) as u64)
    }

    /// Token breakdown (input, output, `cache_read`) since a given timestamp.
    pub async fn token_breakdown_since(&self, since: u64) -> Option<(u64, u64, u64)> {
        let mut rows = self
            .conn
            .query(
                "SELECT COALESCE(SUM(input_tokens), 0),
                        COALESCE(SUM(output_tokens), 0),
                        COALESCE(SUM(cache_read_tokens), 0)
                 FROM turns WHERE timestamp >= ?1",
                params![since as i64],
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        Some((
            row.get::<i64>(0).unwrap_or(0) as u64,
            row.get::<i64>(1).unwrap_or(0) as u64,
            row.get::<i64>(2).unwrap_or(0) as u64,
        ))
    }

    /// Cost grouped by model since a given timestamp.
    /// Returns `(model, cost, total_tokens)`.
    pub async fn cost_by_model_since(&self, since: u64) -> Vec<(String, f64, u64)> {
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT model, SUM(cost_usd), SUM(input_tokens + output_tokens)
                 FROM turns WHERE timestamp >= ?1
                 GROUP BY model ORDER BY SUM(cost_usd) DESC",
                params![since as i64],
            )
            .await
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            let model: String = row.get(0).unwrap_or_default();
            let cost: f64 = row.get(1).unwrap_or(0.0);
            let tokens: i64 = row.get(2).unwrap_or(0);
            out.push((model, cost, tokens as u64));
        }
        out
    }

    /// Cost grouped by category since a given timestamp.
    /// Returns `(category, cost, turn_count)`.
    pub async fn cost_by_category_since(&self, since: u64) -> Vec<(String, f64, u64)> {
        let Ok(mut rows) = self
            .conn
            .query(
                "SELECT category, SUM(cost_usd), COUNT(*)
                 FROM turns WHERE timestamp >= ?1
                 GROUP BY category ORDER BY SUM(cost_usd) DESC",
                params![since as i64],
            )
            .await
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(Some(row)) = rows.next().await {
            let cat: String = row.get(0).unwrap_or_default();
            let cost: f64 = row.get(1).unwrap_or(0.0);
            let count: i64 = row.get(2).unwrap_or(0);
            out.push((cat, cost, count as u64));
        }
        out
    }

    // ── Accounting: parse_offsets table ────────────────────────────────

    /// Get the saved parse offset for a JSONL file.
    /// Returns `(byte_offset, mtime)` or `None` if not tracked.
    pub async fn get_parse_offset(&self, path: &str) -> Option<(u64, u64)> {
        let mut rows = self
            .conn
            .query(
                "SELECT byte_offset, mtime FROM parse_offsets WHERE file_path = ?1",
                params![path],
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        let offset: i64 = row.get(0).ok()?;
        let mtime: i64 = row.get(1).ok()?;
        Some((offset as u64, mtime as u64))
    }

    /// Save the parse offset for a JSONL file. Best-effort.
    pub async fn set_parse_offset(&self, path: &str, offset: u64, mtime: u64) {
        let _ = self
            .conn
            .execute(
                "INSERT INTO parse_offsets (file_path, byte_offset, mtime) VALUES (?1, ?2, ?3)
                 ON CONFLICT(file_path) DO UPDATE SET byte_offset = ?2, mtime = ?3",
                params![path, offset as i64, mtime as i64],
            )
            .await;
    }

    /// Checkpoints the WAL. Best-effort.
    pub async fn checkpoint(&self) {
        let _ = self
            .conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_column_migration_tolerates_duplicate_column_race() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("sessions.db");
        let db = Builder::new_local(&db_path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
            );",
        )
        .await
        .unwrap();

        assert!(!session_column_exists(&conn, "parent_session_id").await);

        conn.execute("ALTER TABLE sessions ADD COLUMN parent_session_id TEXT", ())
            .await
            .unwrap();

        assert!(add_session_parent_column_after_missing_check(
            &conn,
            "parent_session_id",
            "ALTER TABLE sessions ADD COLUMN parent_session_id TEXT",
        )
        .await
        .is_some());
    }
}
