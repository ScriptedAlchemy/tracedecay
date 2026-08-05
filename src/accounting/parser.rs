//! JSONL session parser for Claude Code transcripts.
//!
//! Reads `~/.claude/projects/**/*.jsonl`, extracts assistant turns with
//! model/usage/tool data, and inserts them into the `turns` table via the
//! retained `RegisteredGlobalDb`. Uses offset tracking for incremental re-parsing.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::accounting::classifier;
use crate::accounting::pricing;
use crate::global_db::RegisteredGlobalDb;
use crate::types::CostTurn;

/// Find all JSONL session files under `~/.claude/projects/`.
fn find_session_files() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    find_session_files_at(&home)
}

/// Find all Claude transcript files below one admitted host-data home.
fn find_session_files_at(home: &Path) -> Vec<PathBuf> {
    let projects_dir = home.join(".claude").join("projects");
    if !projects_dir.is_dir() {
        return Vec::new();
    }

    let mut files = Vec::new();
    collect_jsonl_files(&projects_dir, &mut files, 0);
    files
}

/// Recursively collect .jsonl files, with a depth limit to avoid runaway traversal.
fn collect_jsonl_files(dir: &Path, out: &mut Vec<PathBuf>, depth: u8) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, out, depth + 1);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

/// Extract project hash and session ID from a JSONL file path.
/// Path pattern: `~/.claude/projects/<project-hash>/<session-id>.jsonl`
/// or `~/.claude/projects/<project-hash>/<session-id>/subagents/<agent>.jsonl`
fn extract_path_parts(path: &Path) -> (String, String) {
    let components: Vec<&str> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();

    // Find "projects" in the path and take the next component as project_hash
    let projects_idx = components.iter().position(|c| *c == "projects");
    let project_hash = projects_idx
        .and_then(|i| components.get(i + 1))
        .unwrap_or(&"unknown")
        .to_string();

    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    (project_hash, session_id)
}

/// Parse a single JSONL line into a `CostTurn`, if it's an assistant message
/// with usage data.
fn parse_line(line: &str, project_hash: &str, session_id: &str) -> Option<CostTurn> {
    let v: Value = serde_json::from_str(line).ok()?;

    // Only process assistant messages
    if v.get("type")?.as_str()? != "assistant" {
        return None;
    }

    let msg = v.get("message")?;
    let message_id = msg.get("id")?.as_str()?;
    let model = msg.get("model")?.as_str()?;

    let usage = msg.get("usage")?;
    let input_tokens = usage.get("input_tokens")?.as_u64().unwrap_or(0);
    let output_tokens = usage.get("output_tokens")?.as_u64().unwrap_or(0);
    let cache_write_tokens = usage
        .get("cache_creation_input_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let cache_read_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    // Parse timestamp from the outer object (ISO 8601)
    let timestamp = parse_timestamp(v.get("timestamp")?.as_str()?)?;

    // Extract tool names and bash commands for classification
    let content = msg.get("content").and_then(|c| c.as_array());
    let mut tool_names_vec: Vec<String> = Vec::new();
    let mut bash_commands: Vec<String> = Vec::new();

    if let Some(blocks) = content {
        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                && let Some(name) = block.get("name").and_then(|n| n.as_str())
            {
                tool_names_vec.push(name.to_string());
                if name == "Bash"
                    && let Some(cmd) = block
                        .get("input")
                        .and_then(|i| i.get("command"))
                        .and_then(|c| c.as_str())
                {
                    bash_commands.push(cmd.to_string());
                }
            }
        }
    }

    // Classify
    let tool_refs: Vec<&str> = tool_names_vec
        .iter()
        .map(std::string::String::as_str)
        .collect();
    let bash_refs: Vec<&str> = bash_commands
        .iter()
        .map(std::string::String::as_str)
        .collect();
    let category = classifier::classify(&tool_refs, &bash_refs);

    // Compute cost
    let cost_usd = pricing::cost_of_turn(
        model,
        input_tokens,
        output_tokens,
        cache_write_tokens,
        cache_read_tokens,
    );

    Some(CostTurn {
        message_id: message_id.to_string(),
        project_hash: project_hash.to_string(),
        session_id: session_id.to_string(),
        model: model.to_string(),
        timestamp,
        input_tokens,
        output_tokens,
        cache_write_tokens,
        cache_read_tokens,
        cost_usd,
        category: category.as_str().to_string(),
        tool_names: tool_names_vec.join(","),
    })
}

/// Parse an ISO 8601 / RFC3339 timestamp (e.g. `2026-04-14T10:32:15.039Z`)
/// to unix epoch seconds via the shared zero-dependency parser, which also
/// validates calendar fields and applies explicit `±HH:MM` offsets.
pub(crate) fn parse_timestamp(ts: &str) -> Option<u64> {
    let secs = crate::timeutil::parse_rfc3339_timestamp(ts)?;
    u64::try_from(secs).ok()
}

/// Stats returned by the `ingest` function.
pub struct IngestStats {
    /// Number of new turns inserted.
    pub turns_inserted: u64,
    /// Total cost of the newly-inserted turns.
    pub cost_usd: f64,
    /// Total input + output tokens of the newly-inserted turns.
    pub tokens_consumed: u64,
    /// Sources whose parsed turns and cursor failed to commit atomically.
    pub sources_failed: u64,
}

/// Ingest all Claude Code session files into the global DB.
/// Uses offset tracking to only parse new lines since the last run.
pub(crate) async fn ingest(gdb: &RegisteredGlobalDb) -> IngestStats {
    let files = match tokio::task::spawn_blocking(find_session_files).await {
        Ok(files) => files,
        Err(error) => {
            tracing::warn!(%error, "accounting transcript discovery failed");
            return failed_ingest();
        }
    };
    ingest_files(gdb, &files).await
}

/// Ingest only transcripts below the daemon-admitted host-data home.
pub(crate) async fn ingest_at(
    gdb: &RegisteredGlobalDb,
    transcript_source_home: &Path,
) -> IngestStats {
    let transcript_source_home = transcript_source_home.to_path_buf();
    let files =
        match tokio::task::spawn_blocking(move || find_session_files_at(&transcript_source_home))
            .await
        {
            Ok(files) => files,
            Err(error) => {
                tracing::warn!(%error, "accounting transcript discovery failed");
                return failed_ingest();
            }
        };
    ingest_files(gdb, &files).await
}

const fn failed_ingest() -> IngestStats {
    IngestStats {
        turns_inserted: 0,
        cost_usd: 0.0,
        tokens_consumed: 0,
        sources_failed: 1,
    }
}

async fn read_exact_window(
    file: &mut tokio::fs::File,
    path: &Path,
    offset: u64,
    length: u64,
) -> std::result::Result<Vec<u8>, String> {
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|error| format!("seek {}: {error}", path.display()))?;
    let length = usize::try_from(length)
        .map_err(|_| format!("read window for {} is too large", path.display()))?;
    let mut bytes = vec![0_u8; length];
    file.read_exact(&mut bytes)
        .await
        .map_err(|error| format!("read {} cursor window: {error}", path.display()))?;
    Ok(bytes)
}

async fn ingest_files(gdb: &RegisteredGlobalDb, files: &[PathBuf]) -> IngestStats {
    let mut total_inserted = 0u64;
    let mut total_cost = 0.0f64;
    let mut total_tokens = 0u64;
    let mut sources_failed = 0u64;

    for file_path in files {
        let cursor_key = accounting_cursor_key(file_path);

        let file = match tokio::fs::File::open(file_path).await {
            Ok(file) => file,
            Err(error) => {
                sources_failed = sources_failed.saturating_add(1);
                tracing::warn!(
                    path = %file_path.display(),
                    %error,
                    "accounting transcript source could not be opened"
                );
                continue;
            }
        };
        let file = file.into_std().await;
        let Ok(meta) = file.metadata() else {
            sources_failed = sources_failed.saturating_add(1);
            continue;
        };
        let file_id = match tracedecay_runtime_core::db::file_generation_identity(&file, file_path)
        {
            Ok(file_id) => file_id,
            Err(error) => {
                sources_failed = sources_failed.saturating_add(1);
                tracing::warn!(
                    path = %file_path.display(),
                    ?error,
                    "accounting transcript source identity is unavailable"
                );
                continue;
            }
        };
        let mut file = tokio::fs::File::from_std(file);
        let leading = match read_exact_window(&mut file, file_path, 0, meta.len().min(4096)).await {
            Ok(leading) => leading,
            Err(error) => {
                sources_failed = sources_failed.saturating_add(1);
                tracing::warn!(
                    path = %file_path.display(),
                    %error,
                    "accounting transcript leading cursor window could not be read"
                );
                continue;
            }
        };

        let prev = match gdb.get_parse_offset_result(&cursor_key).await {
            Ok(Some(previous)) => previous,
            Ok(None) => crate::global_db::ParseOffset::default(),
            Err(error) => {
                sources_failed = sources_failed.saturating_add(1);
                tracing::warn!(
                    path = %file_path.display(),
                    %error,
                    "accounting transcript cursor could not be read"
                );
                continue;
            }
        };
        let seek_to = if prev.file_id == file_id && prev.byte_offset <= meta.len() {
            let trailing = if prev.byte_offset > 4096 {
                match read_exact_window(&mut file, file_path, prev.byte_offset - 4096, 4096).await {
                    Ok(trailing) => trailing,
                    Err(error) => {
                        sources_failed = sources_failed.saturating_add(1);
                        tracing::warn!(
                            path = %file_path.display(),
                            %error,
                            "accounting transcript trailing cursor window could not be read"
                        );
                        continue;
                    }
                }
            } else {
                Vec::new()
            };
            let leading_len = match usize::try_from(prev.byte_offset.min(4096)) {
                Ok(length) => length,
                Err(error) => {
                    sources_failed = sources_failed.saturating_add(1);
                    tracing::warn!(
                        path = %file_path.display(),
                        %error,
                        "accounting transcript cursor window is invalid"
                    );
                    continue;
                }
            };
            let Some(expected_leading) = leading.get(..leading_len) else {
                sources_failed = sources_failed.saturating_add(1);
                tracing::warn!(
                    path = %file_path.display(),
                    "accounting transcript cursor leading window is incomplete"
                );
                continue;
            };
            match tracedecay_runtime_core::db::resume_fingerprint_from_windows(
                prev.byte_offset,
                expected_leading,
                &trailing,
            ) {
                Ok(fingerprint) if fingerprint == prev.mtime => prev.byte_offset,
                Ok(_) => 0,
                Err(error) => {
                    sources_failed = sources_failed.saturating_add(1);
                    tracing::warn!(
                        path = %file_path.display(),
                        ?error,
                        "accounting transcript cursor could not verify its content anchor"
                    );
                    continue;
                }
            }
        } else {
            // A replacement or truncation is a new source generation.
            0
        };
        if seek_to == meta.len() {
            continue;
        }

        let (project_hash, session_id) = extract_path_parts(file_path);
        let mut turns = Vec::new();

        let read_start = seek_to.saturating_sub(4096);
        if file
            .seek(std::io::SeekFrom::Start(read_start))
            .await
            .is_err()
        {
            sources_failed = sources_failed.saturating_add(1);
            continue;
        }
        let mut captured = Vec::new();
        if let Err(error) = file.read_to_end(&mut captured).await {
            sources_failed = sources_failed.saturating_add(1);
            tracing::warn!(
                path = %file_path.display(),
                %error,
                "accounting transcript source could not be read"
            );
            continue;
        }
        let parse_start = match usize::try_from(seek_to.saturating_sub(read_start)) {
            Ok(parse_start) if parse_start <= captured.len() => parse_start,
            _ => {
                sources_failed = sources_failed.saturating_add(1);
                continue;
            }
        };
        let text = match std::str::from_utf8(&captured[parse_start..]) {
            Ok(text) => text,
            Err(error) => {
                sources_failed = sources_failed.saturating_add(1);
                tracing::warn!(
                    path = %file_path.display(),
                    %error,
                    "accounting transcript source is not UTF-8"
                );
                continue;
            }
        };
        let consumed = text.rfind('\n').map_or(0, |index| index + 1);
        let current_offset = seek_to.saturating_add(consumed as u64);
        for (index, line) in text[..consumed].split_inclusive('\n').enumerate() {
            let trimmed = line.trim();
            if !trimmed.is_empty()
                && let Some(turn) = parse_line(trimmed, &project_hash, &session_id)
            {
                turns.push(turn);
            }
            if index % 256 == 255 {
                tokio::task::yield_now().await;
            }
        }

        let resume_fingerprint = match tracedecay_runtime_core::db::resume_fingerprint_from_capture(
            current_offset,
            &leading,
            read_start,
            &captured,
        ) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                sources_failed = sources_failed.saturating_add(1);
                tracing::warn!(
                    path = %file_path.display(),
                    ?error,
                    "accounting transcript cursor could not anchor its committed frontier"
                );
                continue;
            }
        };
        // Commit parsed turns, physical identity, content anchor, and frontier
        // together so retry, replacement, and in-place rewrite stay exact.
        match gdb
            .insert_turns_with_cursor(
                &turns,
                &cursor_key,
                prev,
                crate::global_db::ParseOffset {
                    byte_offset: current_offset,
                    mtime: resume_fingerprint,
                    file_id,
                },
            )
            .await
        {
            Ok((inserted, cost, tokens)) => {
                total_inserted = total_inserted.saturating_add(inserted as u64);
                total_cost += cost;
                total_tokens = total_tokens.saturating_add(tokens);
            }
            Err(error) => {
                sources_failed = sources_failed.saturating_add(1);
                tracing::warn!(
                    path = %file_path.display(),
                    %error,
                    "accounting transcript import rolled back"
                );
            }
        }
    }

    IngestStats {
        turns_inserted: total_inserted,
        cost_usd: total_cost,
        tokens_consumed: total_tokens,
        sources_failed,
    }
}

fn accounting_cursor_key(path: &Path) -> String {
    format!("accounting_turns:{}", path.display())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::io::{Seek, Write};

    use super::*;

    #[test]
    fn test_parse_timestamp() {
        // 2026-01-01T00:00:00Z
        let ts = parse_timestamp("2026-01-01T00:00:00.000Z");
        assert!(ts.is_some());
        let epoch = ts.unwrap();
        // 2026-01-01 = 56 years from 1970, roughly 20454 days
        assert!(epoch > 1_700_000_000);
        assert!(epoch < 1_800_000_000);
    }

    #[test]
    fn test_parse_timestamp_invalid() {
        assert!(parse_timestamp("bad").is_none());
        assert!(parse_timestamp("").is_none());
    }

    #[test]
    fn test_extract_path_parts() {
        let path =
            PathBuf::from("/Users/test/.claude/projects/-Users-test-Code/abc123-session.jsonl");
        let (project, session) = extract_path_parts(&path);
        assert_eq!(project, "-Users-test-Code");
        assert_eq!(session, "abc123-session");
    }

    #[test]
    fn test_parse_line_assistant() {
        let line = r#"{"type":"assistant","message":{"id":"msg_01abc","model":"claude-opus-4-6","role":"assistant","usage":{"input_tokens":1000,"output_tokens":200,"cache_creation_input_tokens":500,"cache_read_input_tokens":800},"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"test.rs"}}]},"timestamp":"2026-04-14T10:00:00.000Z"}"#;
        let turn = parse_line(line, "proj", "sess");
        assert!(turn.is_some());
        let t = turn.unwrap();
        assert_eq!(t.message_id, "msg_01abc");
        assert_eq!(t.model, "claude-opus-4-6");
        assert_eq!(t.input_tokens, 1000);
        assert_eq!(t.output_tokens, 200);
        assert_eq!(t.cache_write_tokens, 500);
        assert_eq!(t.cache_read_tokens, 800);
        assert_eq!(t.category, "coding");
        assert_eq!(t.tool_names, "Edit");
        assert!(t.cost_usd > 0.0);
    }

    #[test]
    fn test_parse_line_user_skipped() {
        let line = r#"{"type":"user","message":{"content":"hello"},"timestamp":"2026-04-14T10:00:00.000Z"}"#;
        assert!(parse_line(line, "proj", "sess").is_none());
    }

    #[test]
    fn test_parse_line_malformed() {
        assert!(parse_line("not json at all", "proj", "sess").is_none());
        assert!(parse_line("{}", "proj", "sess").is_none());
    }

    #[tokio::test]
    async fn ingest_retains_frontier_before_incomplete_final_record() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "accounting-incomplete-record",
        )
        .await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let first = r#"{"type":"assistant","message":{"id":"msg_complete","model":"claude-opus-4-6","role":"assistant","usage":{"input_tokens":1,"output_tokens":1},"content":[]},"timestamp":"2026-04-14T10:00:00.000Z"}"#;
        let second = r#"{"type":"assistant","message":{"id":"msg_incomplete","model":"claude-opus-4-6","role":"assistant","usage":{"input_tokens":2,"output_tokens":2},"content":[]},"timestamp":"2026-04-14T10:00:01.000Z"}"#;
        std::fs::write(&path, format!("{first}\n{second}")).unwrap();

        let first_ingest = ingest_files(&harness.registered, std::slice::from_ref(&path)).await;
        assert_eq!(first_ingest.turns_inserted, 1);
        let cursor_key = accounting_cursor_key(&path);
        let cursor = harness
            .registered
            .get_parse_offset_result(&cursor_key)
            .await
            .unwrap()
            .expect("durable cursor");
        assert_eq!(cursor.byte_offset, first.len() as u64 + 1);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file).unwrap();
        let second_ingest = ingest_files(&harness.registered, std::slice::from_ref(&path)).await;
        assert_eq!(second_ingest.turns_inserted, 1);
    }

    #[tokio::test]
    async fn ingest_restarts_after_same_file_rewrite_past_old_frontier() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "accounting-same-file-rewrite",
        )
        .await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let initial = r#"{"type":"assistant","message":{"id":"msg_initial","model":"claude-opus-4-6","role":"assistant","usage":{"input_tokens":1,"output_tokens":1},"content":[]},"timestamp":"2026-04-14T10:00:00.000Z"}"#.to_owned() + "\n";
        std::fs::write(&path, &initial).unwrap();
        let first = ingest_files(&harness.registered, std::slice::from_ref(&path)).await;
        assert_eq!(first.turns_inserted, 1);

        let replacement = format!(
            "{}\n",
            r#"{"type":"assistant","message":{"id":"msg_replacement","model":"claude-opus-4-6","role":"assistant","usage":{"input_tokens":2,"output_tokens":2},"content":[{"type":"text","text":"replacement replacement replacement replacement"}]},"timestamp":"2026-04-14T10:00:01.000Z"}"#
        );
        assert!(replacement.len() >= initial.len());
        let mut retained = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        retained.set_len(0).unwrap();
        retained.seek(std::io::SeekFrom::Start(0)).unwrap();
        retained.write_all(replacement.as_bytes()).unwrap();
        retained.flush().unwrap();

        let second = ingest_files(&harness.registered, std::slice::from_ref(&path)).await;
        assert_eq!(second.turns_inserted, 1);
        assert_eq!(second.tokens_consumed, 4);
    }
}
