use serde_json::{Value, json};

pub const HOOK_ANALYTICS_WINDOW_ROWS: usize = 10_000;
const HOOK_ANALYTICS_TAIL_CHUNK_BYTES: u64 = 1 << 20;

#[derive(Default)]
pub struct HookAnalyticsWindow {
    pub window_rows: usize,
    pub rows_scanned: i64,
    pub truncated: bool,
}

pub struct HookAnalyticsRows {
    pub rows: Vec<Value>,
    pub sources: Vec<Value>,
    pub window: HookAnalyticsWindow,
}

pub struct HookAnalyticsReadFailure {
    pub path: std::path::PathBuf,
    pub reason: String,
}

pub struct HookAnalyticsReadOutcome {
    pub rows: HookAnalyticsRows,
    pub sources_attempted: usize,
    pub failures: Vec<HookAnalyticsReadFailure>,
}

impl HookAnalyticsRows {
    pub(super) fn empty() -> Self {
        Self {
            rows: Vec::new(),
            sources: Vec::new(),
            window: HookAnalyticsWindow {
                window_rows: HOOK_ANALYTICS_WINDOW_ROWS,
                rows_scanned: 0,
                truncated: false,
            },
        }
    }

    pub(super) fn window_payload(&self) -> Value {
        let timestamps = || {
            self.rows
                .iter()
                .filter_map(|row| row.get("ts_unix_ms").and_then(Value::as_i64))
        };
        json!({
            "window_rows": self.window.window_rows as i64,
            "rows_scanned": self.window.rows_scanned,
            "rows_included": self.rows.len() as i64,
            "truncated": self.window.truncated,
            "total_rows_known": !self.window.truncated,
            "oldest_ts_unix_ms": timestamps().min(),
            "newest_ts_unix_ms": timestamps().max(),
        })
    }
}

pub fn read_hook_analytics_rows_at(
    store_root: Option<&std::path::Path>,
    project_root: Option<&std::path::Path>,
) -> HookAnalyticsRows {
    let profile_path = tracedecay_runtime_core::storage::default_profile_root()
        .ok()
        .map(|root| root.join("hook_analytics.jsonl"));
    read_hook_analytics_rows_from_paths(store_root, profile_path.as_deref(), project_root)
}

pub fn read_hook_analytics_rows_from_paths(
    store_root: Option<&std::path::Path>,
    profile_hook_path: Option<&std::path::Path>,
    project_root: Option<&std::path::Path>,
) -> HookAnalyticsRows {
    read_hook_analytics_rows_from_paths_checked(store_root, profile_hook_path, project_root).rows
}

pub fn read_hook_analytics_rows_from_paths_checked(
    store_root: Option<&std::path::Path>,
    profile_hook_path: Option<&std::path::Path>,
    project_root: Option<&std::path::Path>,
) -> HookAnalyticsReadOutcome {
    let mut out = HookAnalyticsRows::empty();
    let mut sources_attempted = 0;
    let mut failures = Vec::new();
    let store_path = store_root.map(|root| root.join("hook_analytics.jsonl"));
    if let Some(store_path) = &store_path {
        sources_attempted += 1;
        if let Err(error) = read_hook_analytics_file(store_path, None, &mut out) {
            failures.push(HookAnalyticsReadFailure {
                path: store_path.clone(),
                reason: error.to_string(),
            });
        }
    }
    if let Some(profile_hook_path) = profile_hook_path
        && store_path.as_deref() != Some(profile_hook_path)
    {
        sources_attempted += 1;
        if let Err(error) = read_hook_analytics_file(profile_hook_path, project_root, &mut out) {
            failures.push(HookAnalyticsReadFailure {
                path: profile_hook_path.to_path_buf(),
                reason: error.to_string(),
            });
        }
    }
    sort_hook_analytics_rows(&mut out.rows);
    HookAnalyticsReadOutcome {
        rows: out,
        sources_attempted,
        failures,
    }
}

pub(super) fn sort_hook_analytics_rows(rows: &mut [Value]) {
    rows.sort_by(|left, right| {
        hook_analytics_row_order_key(left).cmp(&hook_analytics_row_order_key(right))
    });
}

fn hook_analytics_row_order_key(row: &Value) -> (i64, &str, &str, &str) {
    (
        row.get("ts_unix_ms")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        row.get("session_id").and_then(Value::as_str).unwrap_or(""),
        row.get("hook_name").and_then(Value::as_str).unwrap_or(""),
        row.get("agent").and_then(Value::as_str).unwrap_or(""),
    )
}

fn read_hook_analytics_tail(
    path: &std::path::Path,
    window_rows: usize,
) -> std::io::Result<(Vec<String>, bool)> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let mut end = file.metadata()?.len();
    let mut buffer: Vec<u8> = Vec::new();
    let mut reached_file_start = true;
    let mut starts_at_line_boundary = true;
    while end > 0 {
        let chunk_len = HOOK_ANALYTICS_TAIL_CHUNK_BYTES.min(end);
        let start = end - chunk_len;
        let chunk_len = usize::try_from(chunk_len)
            .map_err(|_| std::io::Error::other("hook analytics tail chunk is too large"))?;
        let mut chunk = vec![0u8; chunk_len];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buffer);
        buffer = chunk;
        end = start;
        if end > 0 && bytecount(&buffer, b'\n') >= window_rows {
            reached_file_start = false;
            file.seek(SeekFrom::Start(end.saturating_sub(1)))?;
            let mut preceding = [0_u8; 1];
            file.read_exact(&mut preceding)?;
            starts_at_line_boundary = preceding[0] == b'\n';
            break;
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    let mut lines: Vec<&str> = text.lines().collect();
    if !reached_file_start && !starts_at_line_boundary && !lines.is_empty() {
        lines.remove(0);
    }
    if lines.len() > window_rows {
        reached_file_start = false;
        lines.drain(..lines.len() - window_rows);
    }
    Ok((
        lines.into_iter().map(str::to_string).collect(),
        reached_file_start,
    ))
}

fn bytecount(haystack: &[u8], needle: u8) -> usize {
    let mut count = 0;
    let mut remaining = haystack;
    while let Some(index) = remaining.iter().position(|byte| *byte == needle) {
        count += 1;
        remaining = &remaining[index + 1..];
    }
    count
}

pub(super) fn read_hook_analytics_file(
    path: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut HookAnalyticsRows,
) -> std::io::Result<()> {
    let window_rows = out.window.window_rows;
    let (lines, reached_file_start) = read_hook_analytics_tail(path, window_rows)?;
    let rows_scanned = lines.len() as i64;
    let mut rows_total = 0i64;
    let mut rows_included = 0i64;
    let mut rows_malformed = 0i64;
    let mut first_malformed_offset = None;
    let mut first_malformed_error = None;
    for (index, line) in lines.iter().enumerate() {
        let row = match serde_json::from_str::<Value>(line) {
            Ok(row) => row,
            Err(err) => {
                rows_malformed += 1;
                if first_malformed_offset.is_none() {
                    first_malformed_offset = Some(index + 1);
                    first_malformed_error = Some(err.to_string());
                }
                tracing::warn!(
                    hook_analytics_path = %path.display(),
                    window_line_number = index + 1,
                    error = %err,
                    "skipping malformed hook analytics jsonl row"
                );
                continue;
            }
        };
        rows_total += 1;
        let included = match project_filter {
            None => true,
            Some(root) => hook_row_matches_project(&row, root),
        };
        if included {
            rows_included += 1;
            out.rows.push(row);
        }
    }
    out.window.rows_scanned += rows_scanned;
    out.window.truncated |= !reached_file_start;
    out.sources.push(json!({
        "path": path.display().to_string(),
        "rows_scanned": rows_scanned,
        "rows_total": rows_total,
        "rows_included": rows_included,
        "rows_malformed": rows_malformed,
        "window_rows": window_rows as i64,
        "window_truncated": !reached_file_start,
        "first_malformed_line": first_malformed_offset,
        "first_malformed_error": first_malformed_error,
    }));
    Ok(())
}

fn hook_row_matches_project(row: &Value, project_root: &std::path::Path) -> bool {
    ["project_root", "event_cwd"].iter().any(|key| {
        row.get(*key)
            .and_then(Value::as_str)
            .is_some_and(|value| std::path::Path::new(value).starts_with(project_root))
    })
}
