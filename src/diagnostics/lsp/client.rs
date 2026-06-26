use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::diagnostics::lsp::broker::{CodeDiagnostic, DiagnosticSeverity};
use crate::errors::{Result, TraceDecayError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDocument {
    pub language: String,
    pub language_id: String,
    pub relative_path: String,
    pub text: String,
}

pub async fn collect_document_diagnostics(
    command: &str,
    args: &[String],
    project_root: &Path,
    documents: Vec<LspDocument>,
    diagnostics_timeout: Duration,
) -> Result<Vec<CodeDiagnostic>> {
    let mut client = StdioLspClient::start(command, args, project_root).await?;
    client
        .collect_document_diagnostics(project_root, documents, diagnostics_timeout)
        .await
}

pub struct StdioLspClient {
    command: String,
    document_versions: BTreeMap<String, i32>,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    child: tokio::process::Child,
}

impl StdioLspClient {
    pub async fn start(command: &str, args: &[String], project_root: &Path) -> Result<Self> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .current_dir(project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to spawn LSP server '{command}': {e}"),
            })?;

        let mut stdin = child.stdin.take().ok_or_else(|| TraceDecayError::Config {
            message: format!("failed to open stdin for LSP server '{command}'"),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| TraceDecayError::Config {
            message: format!("failed to open stdout for LSP server '{command}'"),
        })?;
        let mut reader = BufReader::new(stdout);

        write_message(
            &mut stdin,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "processId": null,
                    "rootUri": file_uri(project_root),
                    "capabilities": {
                        "textDocument": {
                            "publishDiagnostics": {}
                        }
                    },
                    "workspaceFolders": [{
                        "uri": file_uri(project_root),
                        "name": project_root
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("workspace")
                    }]
                }
            }),
        )
        .await?;
        wait_for_initialize(&mut reader).await?;
        write_message(
            &mut stdin,
            json!({
                "jsonrpc": "2.0",
                "method": "initialized",
                "params": {}
            }),
        )
        .await?;

        Ok(Self {
            command: command.to_string(),
            document_versions: BTreeMap::new(),
            stdin,
            reader,
            child,
        })
    }

    pub async fn collect_document_diagnostics(
        &mut self,
        project_root: &Path,
        documents: Vec<LspDocument>,
        diagnostics_timeout: Duration,
    ) -> Result<Vec<CodeDiagnostic>> {
        let mut uri_to_document = BTreeMap::new();
        for document in &documents {
            let uri = file_uri(&project_root.join(&document.relative_path));
            uri_to_document.insert(uri.clone(), document.clone());
            let next_version = self.document_versions.get(&uri).copied().unwrap_or(0) + 1;
            if next_version == 1 {
                write_message(
                    &mut self.stdin,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/didOpen",
                        "params": {
                            "textDocument": {
                                "uri": uri,
                                "languageId": document.language_id,
                                "version": next_version,
                                "text": document.text,
                            }
                        }
                    }),
                )
                .await?;
            }
            let change_version = next_version + 1;
            write_message(
                &mut self.stdin,
                json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": {
                            "uri": uri,
                            "version": change_version
                        },
                        "contentChanges": [{
                            "text": document.text,
                        }]
                    }
                }),
            )
            .await?;
            self.document_versions.insert(uri, change_version);
        }

        let mut diagnostics_by_uri: BTreeMap<String, Vec<CodeDiagnostic>> = BTreeMap::new();
        let deadline = tokio::time::Instant::now() + diagnostics_timeout;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline.saturating_duration_since(now);
            let message =
                match tokio::time::timeout(remaining, read_message(&mut self.reader)).await {
                    Ok(Ok(Some(message))) => message,
                    Ok(Ok(None)) | Err(_) => break,
                    Ok(Err(err)) => return Err(err),
                };
            if message.method.as_deref() != Some("textDocument/publishDiagnostics") {
                continue;
            }
            let Some(params) = message.params else {
                continue;
            };
            let Ok(published) = serde_json::from_value::<PublishDiagnosticsParams>(params) else {
                continue;
            };
            let Some(document) = uri_to_document.get(&published.uri) else {
                continue;
            };
            diagnostics_by_uri.insert(
                published.uri,
                published
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| diagnostic.into_code_diagnostic(document, &self.command))
                    .collect(),
            );
        }
        Ok(diagnostics_by_uri.into_values().flatten().collect())
    }
}

impl Drop for StdioLspClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

async fn wait_for_initialize(reader: &mut BufReader<tokio::process::ChildStdout>) -> Result<()> {
    loop {
        let Some(message) = read_message(reader).await? else {
            return Err(TraceDecayError::Config {
                message: "LSP server closed before initialize response".to_string(),
            });
        };
        if message.id == Some(json!(1)) {
            return Ok(());
        }
    }
}

async fn write_message(stdin: &mut tokio::process::ChildStdin, value: Value) -> Result<()> {
    let body = serde_json::to_vec(&value).map_err(|e| TraceDecayError::Config {
        message: format!("failed to encode LSP message: {e}"),
    })?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin
        .write_all(header.as_bytes())
        .await
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to write LSP message: {e}"),
        })?;
    stdin
        .write_all(&body)
        .await
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to write LSP message: {e}"),
        })?;
    stdin.flush().await.map_err(|e| TraceDecayError::Config {
        message: format!("failed to flush LSP message: {e}"),
    })
}

async fn read_message(
    reader: &mut BufReader<tokio::process::ChildStdout>,
) -> Result<Option<JsonRpcMessage>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let bytes = reader
            .read_line(&mut line)
            .await
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to read LSP header: {e}"),
            })?;
        if bytes == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        let Some((name, value)) = trimmed.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = content_length else {
        return Err(TraceDecayError::Config {
            message: "LSP message missing Content-Length header".to_string(),
        });
    };
    let mut body = vec![0_u8; length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to read LSP body: {e}"),
        })?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to parse LSP message: {e}"),
        })
}

fn file_uri(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    file_uri_from_path_text(&absolute.to_string_lossy())
}

fn file_uri_from_path_text(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let encoded = percent_encode_file_uri_path(&normalized);
    if normalized.starts_with("//") {
        format!("file:{encoded}")
    } else if looks_like_windows_drive_path(&normalized) {
        format!("file:///{encoded}")
    } else {
        format!("file://{encoded}")
    }
}

fn looks_like_windows_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

fn percent_encode_file_uri_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(*byte as char);
            }
            _ => {
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

#[derive(Debug, Deserialize)]
struct JsonRpcMessage {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct PublishDiagnosticsParams {
    uri: String,
    diagnostics: Vec<LspDiagnostic>,
}

#[derive(Debug, Deserialize)]
struct LspDiagnostic {
    range: LspRange,
    #[serde(default)]
    severity: Option<u8>,
    #[serde(default)]
    code: Option<Value>,
    #[serde(default)]
    source: Option<String>,
    message: String,
}

impl LspDiagnostic {
    fn into_code_diagnostic(self, document: &LspDocument, command: &str) -> CodeDiagnostic {
        CodeDiagnostic {
            language: document.language.clone(),
            source: self.source.unwrap_or_else(|| command.to_string()),
            file: document.relative_path.clone(),
            line_start: self.range.start.line + 1,
            line_end: self.range.end.line + 1,
            character_start: Some(self.range.start.character),
            character_end: Some(self.range.end.character),
            severity: match self.severity {
                Some(1) => DiagnosticSeverity::Error,
                Some(2) => DiagnosticSeverity::Warning,
                Some(4) => DiagnosticSeverity::Hint,
                _ => DiagnosticSeverity::Information,
            },
            code: self.code.and_then(code_to_string),
            message: self.message,
            enclosing_node: None,
            updated_at: now_unix(),
        }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

fn code_to_string(value: Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct LspRange {
    start: LspPosition,
    end: LspPosition,
}

#[derive(Debug, Deserialize)]
struct LspPosition {
    line: u32,
    character: u32,
}

#[cfg(test)]
mod tests {
    use super::file_uri_from_path_text;

    #[test]
    fn file_uri_encodes_lsp_paths() {
        assert_eq!(
            file_uri_from_path_text("/tmp/trace decay/main#one.rs"),
            "file:///tmp/trace%20decay/main%23one.rs"
        );
        assert_eq!(
            file_uri_from_path_text(r"C:\repo with spaces\src\main.rs"),
            "file:///C:/repo%20with%20spaces/src/main.rs"
        );
        assert_eq!(
            file_uri_from_path_text("/tmp/100% real.rs"),
            "file:///tmp/100%25%20real.rs"
        );
    }
}
