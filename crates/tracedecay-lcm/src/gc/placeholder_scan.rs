use tracedecay_runtime_core::db::engine::{QueryExecutor, Value as SqlValue};

use super::{
    GC_PREFIXES, LCM_SCAN_PAGE_MAX_BYTES, LCM_SCAN_PAGE_ROWS, LIVE_PREFIX_REWRITES, LcmError,
};

const PLACEHOLDER_TEXT_COLUMNS: [&str; 4] =
    ["content", "snippet_text", "index_text", "metadata_json"];

pub(crate) enum PlaceholderScanScope<'a> {
    Unscoped,
    ExactProvider {
        provider: &'a str,
        session_id: Option<&'a str>,
    },
    ProviderOrAll {
        provider: &'a str,
        session_id: Option<&'a str>,
    },
}

pub(crate) struct PlaceholderTextRow {
    pub store_id: i64,
    pub content: Option<String>,
    pub snippet_text: String,
    pub index_text: String,
    pub metadata_json: Option<String>,
}

impl PlaceholderTextRow {
    pub(crate) fn texts(&self) -> impl Iterator<Item = &str> {
        self.content
            .as_deref()
            .into_iter()
            .chain(std::iter::once(self.snippet_text.as_str()))
            .chain(std::iter::once(self.index_text.as_str()))
            .chain(self.metadata_json.as_deref())
    }
}

pub(crate) fn gc_prefix_like_patterns() -> Vec<String> {
    GC_PREFIXES
        .iter()
        .map(|prefix| format!("%{prefix}%"))
        .collect()
}

pub(crate) fn gc_prefix_ref_like_patterns(payload_ref: &str) -> Vec<String> {
    GC_PREFIXES
        .iter()
        .map(|prefix| format!("%{prefix}%{payload_ref}%"))
        .collect()
}

/// Prefilter patterns for text that still holds a *live* placeholder naming
/// `payload_ref`.
///
/// A live placeholder is a bracket whose lowercased text starts with one of
/// [`LIVE_PREFIX_REWRITES`] and carries `ref=<payload_ref>` after that prefix,
/// so `%prefix%ref%` matches every row a tombstone rewrite could change. It
/// matches strictly fewer rows than a bare `%ref%`, which also drags in inline
/// bodies and already-tombstoned placeholders that the rewrite leaves alone.
pub(crate) fn live_prefix_ref_like_patterns(payload_ref: &str) -> Vec<String> {
    LIVE_PREFIX_REWRITES
        .iter()
        .map(|(prefix, _)| format!("%{prefix}%{payload_ref}%"))
        .collect()
}

pub(crate) fn live_prefix_like_patterns() -> Vec<String> {
    LIVE_PREFIX_REWRITES
        .iter()
        .map(|(prefix, _)| format!("%{prefix}%"))
        .collect()
}

pub(crate) fn all_placeholder_like_patterns() -> Vec<String> {
    let mut patterns = live_prefix_like_patterns();
    patterns.extend(gc_prefix_like_patterns());
    patterns
}

pub(crate) fn placeholder_text_like_sql(pattern_count: usize) -> String {
    PLACEHOLDER_TEXT_COLUMNS
        .iter()
        .flat_map(|column| {
            (0..pattern_count).map(move |_| format!("{column} LIKE ? COLLATE NOCASE"))
        })
        .collect::<Vec<_>>()
        .join(" OR ")
}

pub(crate) fn bind_placeholder_like_patterns(patterns: &[String]) -> Vec<SqlValue> {
    PLACEHOLDER_TEXT_COLUMNS
        .iter()
        .flat_map(|_| patterns.iter().cloned().map(SqlValue::Text))
        .collect()
}

fn session_sql_value(session_id: Option<&str>) -> SqlValue {
    session_id.map_or(SqlValue::Null, |value| SqlValue::Text(value.to_string()))
}

fn placeholder_scan_scope_sql(scope: &PlaceholderScanScope<'_>) -> (String, Vec<SqlValue>) {
    match scope {
        PlaceholderScanScope::Unscoped => ("1 = 1".to_string(), Vec::new()),
        PlaceholderScanScope::ExactProvider {
            provider,
            session_id,
        } => {
            let session = session_sql_value(*session_id);
            (
                "provider = ? AND (? IS NULL OR session_id = ?)".to_string(),
                vec![
                    SqlValue::Text((*provider).to_string()),
                    session.clone(),
                    session,
                ],
            )
        }
        PlaceholderScanScope::ProviderOrAll {
            provider,
            session_id,
        } => {
            let session = session_sql_value(*session_id);
            (
                "(? = 'all' OR provider = ?) AND (? IS NULL OR session_id = ?)".to_string(),
                vec![
                    SqlValue::Text((*provider).to_string()),
                    SqlValue::Text((*provider).to_string()),
                    session.clone(),
                    session,
                ],
            )
        }
    }
}

/// Whether a keyset scan keeps paging after the visitor saw a row.
enum PlaceholderScanFlow {
    Continue,
    Stop,
}

/// Aggregates every prefiltered candidate row. Callers that only need to know
/// whether *one* row qualifies must use [`any_placeholder_text_row`] instead:
/// this retains the full result in memory.
#[hotpath::measure(label = "sessions.lcm.gc.placeholder_scan", future = true)]
pub(crate) async fn scan_placeholder_text_rows(
    conn: &(impl QueryExecutor + ?Sized),
    scope: PlaceholderScanScope<'_>,
    like_patterns: &[String],
) -> Result<Vec<PlaceholderTextRow>, LcmError> {
    let mut rows_out = Vec::new();
    drive_placeholder_text_scan(conn, scope, like_patterns, |row| {
        rows_out.push(row);
        PlaceholderScanFlow::Continue
    })
    .await?;
    Ok(rows_out)
}

/// Streams the same prefiltered candidate rows and stops at the first row
/// `confirm` accepts, retaining none of them.
///
/// The `LIKE` patterns are only a prefilter, so `confirm` stays the authority
/// over what a match means; the early exit changes when the scan stops, never
/// what it decides. An existence question about one payload therefore costs one
/// page of candidates instead of the store's whole placeholder history.
#[hotpath::measure(label = "sessions.lcm.gc.placeholder_probe", future = true)]
pub(crate) async fn any_placeholder_text_row(
    conn: &(impl QueryExecutor + ?Sized),
    scope: PlaceholderScanScope<'_>,
    like_patterns: &[String],
    mut confirm: impl FnMut(&PlaceholderTextRow) -> bool,
) -> Result<bool, LcmError> {
    let mut confirmed = false;
    drive_placeholder_text_scan(conn, scope, like_patterns, |row| {
        if confirm(&row) {
            confirmed = true;
            PlaceholderScanFlow::Stop
        } else {
            PlaceholderScanFlow::Continue
        }
    })
    .await?;
    Ok(confirmed)
}

async fn drive_placeholder_text_scan(
    conn: &(impl QueryExecutor + ?Sized),
    scope: PlaceholderScanScope<'_>,
    like_patterns: &[String],
    mut visit: impl FnMut(PlaceholderTextRow) -> PlaceholderScanFlow,
) -> Result<(), LcmError> {
    if like_patterns.is_empty() {
        return Ok(());
    }
    let (scope_sql, scope_values) = placeholder_scan_scope_sql(&scope);
    let like_sql = placeholder_text_like_sql(like_patterns.len());
    let like_values = bind_placeholder_like_patterns(like_patterns);
    let mut after_store_id = 0_i64;
    loop {
        let sql = format!(
            "WITH page AS (
                 SELECT store_id, content, snippet_text, index_text, metadata_json
                 FROM lcm_raw_messages
                 WHERE {scope_sql}
                   AND store_id > ?
                   AND ({like_sql})
                 ORDER BY store_id
                 LIMIT ?
             ),
             bounded AS (
                 SELECT store_id, content, snippet_text, index_text, metadata_json,
                        ROW_NUMBER() OVER (ORDER BY store_id) AS page_row,
                        SUM(length(CAST(COALESCE(content, '') AS BLOB))
                            + length(CAST(COALESCE(snippet_text, '') AS BLOB))
                            + length(CAST(COALESCE(index_text, '') AS BLOB))
                            + length(CAST(COALESCE(metadata_json, '') AS BLOB)))
                            OVER (ORDER BY store_id) AS cumulative_bytes
                 FROM page
             )
             SELECT store_id, content, snippet_text, index_text, metadata_json
             FROM bounded
             WHERE cumulative_bytes <= ? OR page_row = 1
             ORDER BY store_id"
        );
        let mut values = scope_values.clone();
        values.push(SqlValue::Integer(after_store_id));
        values.extend(like_values.iter().cloned());
        values.push(SqlValue::Integer(LCM_SCAN_PAGE_ROWS));
        values.push(SqlValue::Integer(LCM_SCAN_PAGE_MAX_BYTES));
        let mut rows = conn.query(&sql, values).await?;
        let mut page_rows = 0_usize;
        while let Some(row) = rows.next().await? {
            let store_id: i64 = row.get(0)?;
            if store_id <= after_store_id {
                return Err(LcmError::Db(
                    "LCM placeholder text scan page did not advance".to_string(),
                ));
            }
            after_store_id = store_id;
            page_rows += 1;
            let visited = PlaceholderTextRow {
                store_id,
                content: row.get(1).unwrap_or(None),
                snippet_text: row.get(2)?,
                index_text: row.get(3)?,
                metadata_json: row.get(4).unwrap_or(None),
            };
            if matches!(visit(visited), PlaceholderScanFlow::Stop) {
                return Ok(());
            }
        }
        drop(rows);
        if page_rows == 0 {
            return Ok(());
        }
    }
}

#[hotpath::measure(label = "sessions.lcm.gc.placeholder_count", future = true)]
pub(crate) async fn count_placeholder_text_rows(
    conn: &(impl QueryExecutor + ?Sized),
    scope: PlaceholderScanScope<'_>,
    like_patterns: &[String],
) -> Result<i64, LcmError> {
    if like_patterns.is_empty() {
        return Ok(0);
    }
    let (scope_sql, scope_values) = placeholder_scan_scope_sql(&scope);
    let like_sql = placeholder_text_like_sql(like_patterns.len());
    let sql = format!(
        "SELECT COUNT(*)
         FROM lcm_raw_messages
         WHERE {scope_sql}
           AND ({like_sql})"
    );
    let mut values = scope_values;
    values.extend(bind_placeholder_like_patterns(like_patterns));
    let mut rows = conn.query(&sql, values).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("placeholder text count returned no rows".to_string()))?;
    row.get(0).map_err(|err| LcmError::Db(err.to_string()))
}
