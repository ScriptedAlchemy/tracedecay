// Rust guideline compliant 2025-10-17
use std::collections::HashMap;

use super::connection::Database;
use crate::errors::{Result, TraceDecayError};
use crate::types::*;

impl Database {
    /// Returns aggregate statistics about the code graph.
    pub async fn get_stats(&self) -> Result<GraphStats> {
        // Single query for all scalar counts: nodes, edges, files, last_updated, total_source_bytes
        let mut counts_rows = self
            .conn()
            .query(
                "SELECT \
                   (SELECT COUNT(*) FROM nodes), \
                   (SELECT COUNT(*) FROM edges), \
                   (SELECT COUNT(*) FROM files), \
                   (SELECT COALESCE(MAX(indexed_at), 0) FROM files), \
                   (SELECT COALESCE(SUM(size), 0) FROM files)",
                (),
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query counts: {e}"),
                operation: "get_stats".to_string(),
            })?;
        let counts_row = counts_rows
            .next()
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to read counts row: {e}"),
                operation: "get_stats".to_string(),
            })?;
        let (node_count, edge_count, file_count, last_updated, total_source_bytes) =
            match counts_row {
                Some(r) => {
                    let nc: i64 = r.get(0).unwrap_or(0);
                    let ec: i64 = r.get(1).unwrap_or(0);
                    let fc: i64 = r.get(2).unwrap_or(0);
                    let lu: i64 = r.get(3).unwrap_or(0);
                    let ts: i64 = r.get(4).unwrap_or(0);
                    (nc as u64, ec as u64, fc as u64, lu as u64, ts as u64)
                }
                None => (0, 0, 0, 0, 0),
            };

        // Nodes grouped by kind
        let nodes_by_kind = query_kind_counts(
            self.conn(),
            "SELECT kind, COUNT(*) FROM nodes GROUP BY kind",
        )
        .await?;

        // Edges grouped by kind
        let edges_by_kind = query_kind_counts(
            self.conn(),
            "SELECT kind, COUNT(*) FROM edges GROUP BY kind",
        )
        .await?;

        let db_size_bytes = self.size().await.unwrap_or(0);

        // Files grouped by language. Done in Rust (not SQL) so the label set
        // stays in sync with the extractor registry without an ever-growing
        // CASE expression. See `display_language_for_path`.
        let files_by_language = {
            let mut rows = self
                .conn()
                .query("SELECT path FROM files", ())
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query files for language stats: {e}"),
                    operation: "get_stats".to_string(),
                })?;
            let mut map: HashMap<String, u64> = HashMap::new();
            while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
                message: format!("failed to read file row: {e}"),
                operation: "get_stats".to_string(),
            })? {
                let path: String = row.get(0).map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read file path: {e}"),
                    operation: "get_stats".to_string(),
                })?;
                *map.entry(display_language_for_path(&path).to_string())
                    .or_insert(0) += 1;
            }
            map
        };

        let last_sync_at = self
            .get_metadata("last_sync_at")
            .await?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let last_full_sync_at = self
            .get_metadata("last_full_sync_at")
            .await?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let last_sync_duration_ms = self
            .get_metadata("last_sync_duration_ms")
            .await?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(GraphStats {
            node_count,
            edge_count,
            file_count,
            nodes_by_kind,
            edges_by_kind,
            db_size_bytes,
            last_updated,
            total_source_bytes,
            files_by_language,
            last_sync_at,
            last_full_sync_at,
            last_sync_duration_ms,
        })
    }

    /// Returns the most recent `indexed_at` timestamp across all files,
    /// or 0 if the files table is empty.
    pub async fn last_index_time(&self) -> Result<i64> {
        query_scalar_i64(
            self.conn(),
            "SELECT COALESCE(MAX(indexed_at), 0) FROM files",
            "last_index_time",
        )
        .await
    }
}

/// Maps a file path to a human-readable language label used in
/// `GraphStats::files_by_language`. Anything we don't recognise lands in
/// `"Other"`. The label set must stay in sync with the language extractors
/// registered in `crate::extraction::LanguageRegistry`; the test
/// `files_by_language_covers_known_extensions` guards the mapping.
fn display_language_for_path(path: &str) -> &'static str {
    // Special-case extensionless files we still recognise by name.
    let basename = path.rsplit('/').next().unwrap_or(path);
    let lower = basename.to_ascii_lowercase();
    if lower == "dockerfile" || lower.starts_with("dockerfile.") {
        return "Dockerfile";
    }
    if lower == "makefile" {
        return "Makefile";
    }
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "Rust",
        "go" => "Go",
        "py" | "pyi" | "pyx" => "Python",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "scala" | "sc" => "Scala",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => "C++",
        "cs" => "C#",
        "fs" | "fsi" | "fsx" => "F#",
        "rb" => "Ruby",
        "php" => "PHP",
        "dart" => "Dart",
        "lua" => "Lua",
        "pl" | "pm" => "Perl",
        "sh" | "bash" => "Bash",
        "ps1" | "psm1" => "PowerShell",
        "nix" => "Nix",
        "zig" => "Zig",
        "proto" => "Protobuf",
        "toml" => "TOML",
        "sql" => "SQL",
        "r" => "R",
        "jl" => "Julia",
        "ex" | "exs" => "Elixir",
        "erl" | "hrl" => "Erlang",
        "hs" => "Haskell",
        "clj" | "cljs" | "cljc" | "edn" => "Clojure",
        "ml" | "mli" => "OCaml",
        "lean" => "Lean",
        "m" | "mm" => "Objective-C",
        "f" | "f90" | "f95" | "f03" | "f08" | "for" => "Fortran",
        "cbl" | "cob" | "cpy" => "COBOL",
        "pas" | "pp" | "dpr" => "Pascal",
        "vb" => "VB.NET",
        "bas" => "BASIC",
        "bat" | "cmd" => "Batch",
        "glsl" | "vert" | "frag" | "comp" | "geom" | "tesc" | "tese" => "GLSL",
        "qnt" => "Quint",
        _ => "Other",
    }
}

/// Executes a `SELECT label, COUNT(*) ... GROUP BY` query and returns
/// the results as a `HashMap<String, u64>`.
async fn query_kind_counts(conn: &libsql::Connection, sql: &str) -> Result<HashMap<String, u64>> {
    let mut map = HashMap::new();
    let mut rows = conn
        .query(sql, ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to query kind counts: {e}"),
            operation: "get_stats".to_string(),
        })?;
    while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
        message: format!("failed to read kind count row: {e}"),
        operation: "get_stats".to_string(),
    })? {
        let kind: String = row.get(0).map_err(|e| TraceDecayError::Database {
            message: format!("failed to read kind: {e}"),
            operation: "get_stats".to_string(),
        })?;
        let count: i64 = row.get(1).map_err(|e| TraceDecayError::Database {
            message: format!("failed to read count: {e}"),
            operation: "get_stats".to_string(),
        })?;
        if count > 0 {
            map.insert(kind, count as u64);
        }
    }
    Ok(map)
}

/// Executes a scalar query returning a single `i64` value.
async fn query_scalar_i64(conn: &libsql::Connection, sql: &str, operation: &str) -> Result<i64> {
    let mut rows = conn
        .query(sql, ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to execute scalar query: {e}"),
            operation: operation.to_string(),
        })?;

    let row = rows
        .next()
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to read scalar row: {e}"),
            operation: operation.to_string(),
        })?
        .ok_or_else(|| TraceDecayError::Database {
            message: "no result from scalar query".to_string(),
            operation: operation.to_string(),
        })?;

    row.get::<i64>(0).map_err(|e| TraceDecayError::Database {
        message: format!("failed to read scalar value: {e}"),
        operation: operation.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::display_language_for_path;

    #[test]
    fn maps_common_extensions_to_named_languages() {
        assert_eq!(display_language_for_path("src/main.rs"), "Rust");
        assert_eq!(display_language_for_path("a/b/foo.py"), "Python");
        assert_eq!(display_language_for_path("foo.pyi"), "Python");
        assert_eq!(display_language_for_path("foo.tsx"), "TypeScript");
        assert_eq!(display_language_for_path("foo.cs"), "C#");
        assert_eq!(display_language_for_path("foo.cpp"), "C++");
        assert_eq!(display_language_for_path("Dockerfile"), "Dockerfile");
        assert_eq!(
            display_language_for_path("docker/Dockerfile.prod"),
            "Dockerfile"
        );
        assert_eq!(display_language_for_path("Makefile"), "Makefile");
        assert_eq!(display_language_for_path("readme.txt"), "Other");
        assert_eq!(display_language_for_path("noext"), "Other");
    }

    #[test]
    fn extension_match_is_case_insensitive() {
        assert_eq!(display_language_for_path("Foo.RS"), "Rust");
        assert_eq!(display_language_for_path("Foo.PY"), "Python");
    }
}
