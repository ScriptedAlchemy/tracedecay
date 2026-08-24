use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::broker::{CodeDiagnostic, DiagnosticSeverity};
use crate::{LspError, LspError as TraceDecayError, Result};

const MIN_MESSAGE_IO_TIMEOUT: Duration = Duration::from_secs(2);
const MIN_INITIALIZE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspRefreshTimeouts {
    refresh: Duration,
    initialize_response: Duration,
    message_io: Duration,
    diagnostics_quiet: Duration,
}

impl LspRefreshTimeouts {
    pub fn new(
        refresh: Duration,
        initialize_response: Duration,
        message_io: Duration,
        diagnostics_quiet: Duration,
    ) -> Self {
        Self {
            refresh,
            initialize_response,
            message_io,
            diagnostics_quiet,
        }
    }

    pub fn from_diagnostics_quiet_window(diagnostics_quiet: Duration) -> Self {
        let message_io = diagnostics_quiet.max(MIN_MESSAGE_IO_TIMEOUT);
        let initialize_response = message_io.max(MIN_INITIALIZE_RESPONSE_TIMEOUT);
        let refresh = diagnostics_quiet.saturating_add(message_io);
        Self {
            refresh,
            initialize_response,
            message_io,
            diagnostics_quiet,
        }
    }
}

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
    diagnostics_quiet_timeout: Duration,
) -> Result<Vec<CodeDiagnostic>> {
    let timeouts = LspRefreshTimeouts::from_diagnostics_quiet_window(diagnostics_quiet_timeout);
    collect_document_diagnostics_with_timeouts(command, args, project_root, documents, timeouts)
        .await
}

pub async fn collect_document_diagnostics_with_timeouts(
    command: &str,
    args: &[String],
    project_root: &Path,
    documents: Vec<LspDocument>,
    timeouts: LspRefreshTimeouts,
) -> Result<Vec<CodeDiagnostic>> {
    let mut client =
        StdioLspClient::start_with_timeouts(command, args, project_root, timeouts).await?;
    client
        .collect_document_diagnostics(project_root, documents, timeouts)
        .await
}

pub struct StdioLspClient {
    command: String,
    document_versions: BTreeMap<String, i32>,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    child: tokio::process::Child,
    stderr_task: JoinHandle<()>,
}

impl StdioLspClient {
    pub async fn start_with_timeouts(
        command: &str,
        args: &[String],
        project_root: &Path,
        timeouts: LspRefreshTimeouts,
    ) -> Result<Self> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .current_dir(project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
        let stderr = child.stderr.take().ok_or_else(|| TraceDecayError::Config {
            message: format!("failed to open stderr for LSP server '{command}'"),
        })?;
        let mut reader = BufReader::new(stdout);
        let stderr_capture = Arc::new(Mutex::new(Vec::new()));
        let stderr_task = spawn_stderr_capture(stderr, Arc::clone(&stderr_capture));

        let send_initialize = write_message_with_timeout(
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
            timeouts.message_io,
        )
        .await;
        // A server that dies immediately can fail the initialize *request*
        // write (broken pipe — on Windows this races the spawn under load)
        // just as easily as the initialize *response* wait. Route both
        // failures through the same stderr-enriched classification so the
        // crash reason (e.g. a toolchain's "unknown binary" complaint) is
        // never dropped.
        let initialize_result = match send_initialize {
            Ok(()) => tokio::time::timeout(
                timeouts.initialize_response,
                wait_for_initialize(&mut reader),
            )
            .await
            .unwrap_or_else(|_| {
                Err(TraceDecayError::Config {
                    message: format!(
                        "LSP server '{command}' initialize timed out after {} ms",
                        timeouts.initialize_response.as_millis()
                    ),
                })
            }),
            Err(err) => Err(err),
        };
        if let Err(err) = initialize_result {
            let _ = child.start_kill();
            let _ = child.wait().await;
            let _ = stderr_task.await;
            let stderr = captured_stderr(&stderr_capture).await;
            return Err(enrich_start_error(command, err, &stderr));
        }
        write_message_with_timeout(
            &mut stdin,
            json!({
                "jsonrpc": "2.0",
                "method": "initialized",
                "params": {}
            }),
            timeouts.message_io,
        )
        .await?;

        Ok(Self {
            command: command.to_string(),
            document_versions: BTreeMap::new(),
            stdin,
            reader,
            child,
            stderr_task,
        })
    }

    pub async fn collect_document_diagnostics(
        &mut self,
        project_root: &Path,
        documents: Vec<LspDocument>,
        timeouts: LspRefreshTimeouts,
    ) -> Result<Vec<CodeDiagnostic>> {
        let mut uri_to_document = BTreeMap::new();
        for document in &documents {
            let uri = file_uri(&project_root.join(&document.relative_path));
            uri_to_document.insert(uri.clone(), document.clone());
            let next_version = self.document_versions.get(&uri).copied().unwrap_or(0) + 1;
            if next_version == 1 {
                write_message_with_timeout(
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
                    timeouts.message_io,
                )
                .await?;
            }
            let change_version = next_version + 1;
            write_message_with_timeout(
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
                timeouts.message_io,
            )
            .await?;
            self.document_versions.insert(uri, change_version);
        }

        let mut diagnostics_by_uri: BTreeMap<String, Vec<CodeDiagnostic>> = BTreeMap::new();
        let quiet_deadline = tokio::time::Instant::now() + timeouts.diagnostics_quiet;
        loop {
            let now = tokio::time::Instant::now();
            if now >= quiet_deadline {
                break;
            }
            let Some(message) =
                read_message_until(&mut self.reader, quiet_deadline, timeouts).await?
            else {
                break;
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
        // Servers that publish empty diagnostics for clean files (rust-analyzer,
        // tsserver) will produce one `publishDiagnostics` per requested URI, so a
        // fully complete batch has `diagnostics_by_uri.len() == uri_to_document.len()`.
        // Servers that suppress empty publishes (only publishing for files WITH
        // problems) never emit for clean files, so those batches look "partial"
        // even though every dirty file reported. To avoid dropping real results in
        // that case, only treat the batch as a genuine timeout when NOTHING arrived
        // (matching the #237 behavior of not recording a genuine timeout as
        // complete); otherwise return the diagnostics that were actually published.
        if diagnostics_by_uri.is_empty() && !uri_to_document.is_empty() {
            return Err(refresh_timed_out(timeouts));
        }
        Ok(diagnostics_by_uri.into_values().flatten().collect())
    }
}

impl Drop for StdioLspClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.stderr_task.abort();
    }
}

fn spawn_stderr_capture(
    mut stderr: tokio::process::ChildStderr,
    capture: Arc<Mutex<Vec<u8>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buffer = [0_u8; 1024];
        while let Ok(bytes_read) = stderr.read(&mut buffer).await {
            if bytes_read == 0 {
                break;
            }
            let mut captured = capture.lock().await;
            let remaining = 8192_usize.saturating_sub(captured.len());
            if remaining > 0 {
                captured.extend_from_slice(&buffer[..bytes_read.min(remaining)]);
            }
        }
    })
}

async fn captured_stderr(capture: &Arc<Mutex<Vec<u8>>>) -> String {
    let captured = capture.lock().await;
    String::from_utf8_lossy(&captured).trim().to_string()
}

fn enrich_start_error(command: &str, err: LspError, stderr: &str) -> LspError {
    if stderr.is_empty() {
        return err;
    }
    TraceDecayError::Config {
        message: format!("{command} failed during initialize: {err}; stderr: {stderr}"),
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

async fn write_message_with_timeout(
    stdin: &mut tokio::process::ChildStdin,
    value: Value,
    timeout: Duration,
) -> Result<()> {
    tokio::time::timeout(timeout, write_message(stdin, value))
        .await
        .map_err(|_| TraceDecayError::Config {
            message: format!(
                "LSP message write timed out after {} ms",
                timeout.as_millis()
            ),
        })?
}

fn refresh_timed_out(timeouts: LspRefreshTimeouts) -> LspError {
    TraceDecayError::Config {
        message: format!(
            "LSP diagnostics collection timed out after {} ms",
            timeouts.refresh.as_millis()
        ),
    }
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

async fn read_message_until(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    deadline: tokio::time::Instant,
    timeouts: LspRefreshTimeouts,
) -> Result<Option<JsonRpcMessage>> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") && !header.ends_with(b"\n\n") {
        let Some(byte) = read_byte_until(reader, deadline, !header.is_empty(), timeouts).await?
        else {
            return Ok(None);
        };
        header.push(byte);
        if header.len() > 16 * 1024 {
            return Err(TraceDecayError::Config {
                message: "LSP message header exceeded 16 KiB".to_string(),
            });
        }
    }

    let header = String::from_utf8_lossy(&header);
    let content_length = header.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    let Some(length) = content_length else {
        return Err(TraceDecayError::Config {
            message: "LSP message missing Content-Length header".to_string(),
        });
    };

    let mut body = vec![0_u8; length];
    let mut read = 0;
    while read < length {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(refresh_timed_out(timeouts));
        }
        let remaining = deadline.saturating_duration_since(now);
        let bytes_read = match tokio::time::timeout(remaining, reader.read(&mut body[read..])).await
        {
            Ok(Ok(bytes_read)) => bytes_read,
            Ok(Err(err)) => {
                return Err(TraceDecayError::Config {
                    message: format!("failed to read LSP body: {err}"),
                });
            }
            Err(_) => return Err(refresh_timed_out(timeouts)),
        };
        if bytes_read == 0 {
            return Err(TraceDecayError::Config {
                message: "LSP server closed before completing message body".to_string(),
            });
        }
        read += bytes_read;
    }

    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to parse LSP message: {e}"),
        })
}

async fn read_byte_until(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    deadline: tokio::time::Instant,
    partial_message: bool,
    timeouts: LspRefreshTimeouts,
) -> Result<Option<u8>> {
    let now = tokio::time::Instant::now();
    if now >= deadline {
        return if partial_message {
            Err(refresh_timed_out(timeouts))
        } else {
            Ok(None)
        };
    }
    let mut byte = [0_u8; 1];
    match tokio::time::timeout(
        deadline.saturating_duration_since(now),
        reader.read(&mut byte),
    )
    .await
    {
        Ok(Ok(0)) if partial_message => Err(TraceDecayError::Config {
            message: "LSP server closed before completing message header".to_string(),
        }),
        Ok(Ok(0)) => Ok(None),
        Ok(Ok(_)) => Ok(Some(byte[0])),
        Ok(Err(err)) => Err(TraceDecayError::Config {
            message: format!("failed to read LSP header: {err}"),
        }),
        Err(_) if partial_message => Err(refresh_timed_out(timeouts)),
        Err(_) => Ok(None),
    }
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

/// Build a `file://` URI from raw path text, normalizing `\` to `/` and
/// percent-encoding. Handles POSIX paths, Windows drive paths (`C:/…`), and UNC
/// (`//server/share`) prefixes. Shared with the Kiro installer.
#[doc(hidden)]
pub fn file_uri_from_path_text(path: &str) -> String {
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
            // The LSP client has no code-graph handle; the enclosing symbol is
            // resolved later via `DiagnosticBroker::resolve_enclosing_nodes`,
            // which has access to the indexed nodes for the file.
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
