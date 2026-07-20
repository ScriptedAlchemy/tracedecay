//! Minimal TraceDecay harness for the search-quality suite: copies the
//! committed corpus snapshot into an isolated temp project, indexes it, and
//! calls tools through the public lib dispatch path
//! (`tracedecay::mcp::handle_tool_call`) — the same dispatch an agent's MCP
//! client uses.

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::{Mutex, MutexGuard};
use tracedecay::tracedecay::TraceDecay;

use crate::common;

/// Serializes env-mutating setup across tests in this suite.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

/// An indexed, isolated copy of the committed corpus snapshot.
///
/// Field order is load-bearing: the graph tears down (checkpoint + leak, the
/// same convention as the MCP suite's `TestTraceDecay`, which avoids the
/// Windows libsql destructor abort for short-lived test graphs) before the
/// storage env guard and the temp dir, and the env lock is released last.
pub(crate) struct CorpusProject {
    cg: Option<TraceDecay>,
    _storage: common::TraceDecayStorageEnvGuard,
    _dir: TempDir,
    _lock: MutexGuard<'static, ()>,
}

impl CorpusProject {
    pub(crate) fn cg(&self) -> &TraceDecay {
        self.cg.as_ref().expect("corpus graph is live")
    }
}

impl Drop for CorpusProject {
    fn drop(&mut self) {
        if let Some(cg) = self.cg.take() {
            let close_thread = std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("corpus teardown runtime");
                runtime.block_on(async {
                    let _ = cg.checkpoint().await;
                });
                std::mem::forget(cg);
            });
            let _ = close_thread.join();
        }
    }
}

/// Copies `src` recursively into `dst`, preserving relative paths.
fn copy_dir(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).unwrap_or_else(|err| panic!("read {}: {err}", src.display())) {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else {
            fs::create_dir_all(dst).unwrap();
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Indexes a fresh temp project containing only the corpus snapshot bytes.
pub(crate) async fn setup_corpus_project(corpus_root: &Path) -> CorpusProject {
    let lock = ENV_LOCK.lock().await;
    let dir = TempDir::new().expect("corpus temp dir");
    let storage = common::isolated_tracedecay_storage(&dir);
    let project = dir.path().join("project");
    fs::create_dir_all(&project).unwrap();
    copy_dir(corpus_root, &project);
    let cg = TraceDecay::init(&project)
        .await
        .expect("init corpus project");
    for attempt in 0..20 {
        match cg.index_all().await {
            Ok(_) => break,
            Err(tracedecay::errors::TraceDecayError::SyncLock { .. }) if attempt < 19 => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(err) => panic!("failed to index corpus fixture: {err}"),
        }
    }
    CorpusProject {
        cg: Some(cg),
        _storage: storage,
        _dir: dir,
        _lock: lock,
    }
}

/// Calls one tool through the public dispatch path and returns its parsed
/// JSON payload (`content[0].text` as JSON).
pub(crate) async fn call_tool(cg: &TraceDecay, tool: &str, mut args: Value) -> Value {
    if let Some(obj) = args.as_object_mut() {
        obj.entry("format".to_string())
            .or_insert_with(|| Value::from("json"));
    }
    let result = tracedecay::mcp::handle_tool_call(cg, tool, args, None, None)
        .await
        .unwrap_or_else(|err| panic!("{tool} call failed: {err}"));
    extract_json(&result.value)
}

fn extract_json(value: &Value) -> Value {
    let text = value["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("missing text content in {value}"));
    serde_json::from_str(text).unwrap_or_else(|err| panic!("tool text is not JSON ({err}): {text}"))
}
