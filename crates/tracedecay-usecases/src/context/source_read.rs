use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use super::read_cache::{self, GLOBAL_SESSION};
use super::read_modes::{
    self, LineRange, ReadMode, render_full, render_lines, render_map, render_signatures,
    render_symbol_context,
};
use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::storage::ProjectPath;

pub struct SourceReadRequest<'a> {
    pub file: &'a str,
    pub mode: ReadMode,
    pub line_range: Option<LineRange>,
    pub raw_lines: Option<&'a str>,
    pub include_symbols: bool,
    pub project_id: &'a str,
}

pub struct SourceReadOutput {
    pub file: String,
    pub mode: ReadMode,
    pub mtime_ns: i64,
    pub digest: String,
    pub token_count: u32,
    pub unchanged: bool,
    pub body: Option<String>,
    pub context: Option<Value>,
}

pub async fn read_source(
    graph: &TraceDecay,
    request: SourceReadRequest<'_>,
) -> Result<SourceReadOutput> {
    let SourceReadRequest {
        file,
        mode,
        line_range,
        raw_lines,
        include_symbols,
        project_id,
    } = request;
    let (absolute_path, display_file) = resolve_indexed_source_file(graph, file).await?;
    let mtime_ns =
        read_cache::file_mtime_ns(&absolute_path).map_err(|error| TraceDecayError::Config {
            message: format!("cannot read file metadata for '{file}': {error}"),
        })?;
    let last_sync_at = if matches!(mode, ReadMode::Map | ReadMode::Signatures) {
        graph
            .db()
            .get_metadata("last_sync_at")
            .await
            .unwrap_or(None)
    } else {
        None
    };
    let args_hash = read_cache::args_hash(&json!({
        "lines": raw_lines,
        "last_sync_at": last_sync_at,
    }))?;

    let cache_connection = graph.db().engine_conn();
    if let Some(cached) = read_cache::get(
        &cache_connection,
        project_id,
        GLOBAL_SESSION,
        &display_file,
        mode.as_str(),
        &args_hash,
        mtime_ns,
    )
    .await?
    {
        return Ok(SourceReadOutput {
            context: source_symbol_context(graph, &display_file, mode, line_range, include_symbols)
                .await?,
            file: display_file,
            mode,
            mtime_ns: cached.mtime_ns,
            digest: cached.digest,
            token_count: cached.token_count,
            unchanged: true,
            body: None,
        });
    }

    let body = match mode {
        ReadMode::Full => render_full(
            &tracedecay_runtime_core::sync::read_source_file(&absolute_path).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("cannot read '{file}': {error}"),
                }
            })?,
        ),
        ReadMode::Lines => render_lines(
            &tracedecay_runtime_core::sync::read_source_file(&absolute_path).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("cannot read '{file}': {error}"),
                }
            })?,
            line_range.ok_or_else(|| TraceDecayError::Config {
                message: "lines mode requires a parsed range".to_owned(),
            })?,
        ),
        ReadMode::Map => {
            serde_json::to_string_pretty(&render_map(graph.db(), &display_file, None).await?)
                .map_err(|error| TraceDecayError::Config {
                    message: format!("cannot render map for '{file}': {error}"),
                })?
        }
        ReadMode::Signatures => {
            serde_json::to_string_pretty(&render_signatures(graph.db(), &display_file).await?)
                .map_err(|error| TraceDecayError::Config {
                    message: format!("cannot render signatures for '{file}': {error}"),
                })?
        }
    };
    let context =
        source_symbol_context(graph, &display_file, mode, line_range, include_symbols).await?;
    let token_count = read_modes::estimate_tokens(&body);
    let digest = read_cache::digest_bytes(body.as_bytes());
    if !graph.is_read_only() {
        read_cache::put(
            graph.db(),
            project_id,
            GLOBAL_SESSION,
            &display_file,
            mtime_ns,
            mode.as_str(),
            &args_hash,
            &digest,
            body.as_bytes(),
            token_count,
        )
        .await?;
    }
    Ok(SourceReadOutput {
        file: display_file,
        mode,
        mtime_ns,
        digest,
        token_count,
        unchanged: false,
        body: Some(body),
        context,
    })
}

pub async fn resolve_indexed_source_file(
    graph: &TraceDecay,
    file: &str,
) -> Result<(PathBuf, String)> {
    if file.contains('\0') {
        return Err(TraceDecayError::Config {
            message: "path contains NUL byte".to_owned(),
        });
    }

    let project_root = graph.project_root().to_path_buf();
    let input = Path::new(file);
    if let Ok(project_path) = ProjectPath::resolve(&project_root, input) {
        let display_file = match relative_source_key(input)? {
            Some(relative) => relative,
            None => project_path.relative_path_string(),
        };
        return Ok((project_path.absolute_path(), display_file));
    }

    let display_file = if let Some(relative) = relative_source_key(input)? {
        relative
    } else if let Some(relative) = absolute_source_key(&project_root, input)? {
        relative
    } else {
        return Err(TraceDecayError::Config {
            message: format!(
                "path '{}' escapes project root '{}'",
                input.display(),
                project_root.display()
            ),
        });
    };
    if graph.get_nodes_by_file(&display_file).await?.is_empty() {
        return Err(TraceDecayError::Config {
            message: format!(
                "path '{}' escapes project root '{}' and is not indexed",
                input.display(),
                project_root.display()
            ),
        });
    }
    let absolute_path = if input.is_absolute() {
        input.to_path_buf()
    } else {
        project_root.join(input)
    };
    Ok((absolute_path, display_file))
}

async fn source_symbol_context(
    graph: &TraceDecay,
    display_file: &str,
    mode: ReadMode,
    line_range: Option<LineRange>,
    include_symbols: bool,
) -> Result<Option<Value>> {
    if !include_symbols || !matches!(mode, ReadMode::Full | ReadMode::Lines) {
        return Ok(None);
    }
    render_symbol_context(graph.db(), display_file, line_range)
        .await
        .map(Some)
}

fn relative_source_key(path: &Path) -> Result<Option<String>> {
    if path.is_absolute() {
        return Ok(None);
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            _ => {
                return Err(TraceDecayError::Config {
                    message: format!("path '{}' contains unsafe components", path.display()),
                });
            }
        }
    }
    if parts.is_empty() {
        return Err(TraceDecayError::Config {
            message: "path must name a project file".to_owned(),
        });
    }
    Ok(Some(parts.join("/")))
}

fn absolute_source_key(project_root: &Path, path: &Path) -> Result<Option<String>> {
    if !path.is_absolute() {
        return Ok(None);
    }
    let Ok(relative) = path.strip_prefix(project_root) else {
        return Ok(None);
    };
    relative_source_key(relative)
}
