//! Before/after harness for Cursor parent-transcript dispatch-model lookups.
//!
//! Builds a synthetic ~20 MB parent transcript (no dispatch for agent `X`),
//! runs 512 lookups that simulate repeated subagent batches, then appends a
//! late dispatch and checks it becomes visible. Also exercises truncate+rewrite
//! and inode-replacement. Prints one JSON object to stdout.
//!
//! ```sh
//! source scripts/hotpath-rustflags.sh
//! HOTPATH_METRICS_SERVER_OFF=1 \
//! HOTPATH_OUTPUT_PATH=/tmp/cursor-dispatch-model-hotpath.json \
//! cargo bench -p tracedecay-sessions --bench cursor_dispatch_model \
//!     --features hotpath-alloc
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_sessions::runtime::cursor::parent_dispatch_model_for_subagent_with_receipt;

const PARENT_SESSION_ID: &str = "P";
const AGENT_ID: &str = "X";
const LATE_MODEL: &str = "cursor-late-model";
const RECORD_COUNT: usize = 5_000;
const TARGET_PARENT_BYTES: u64 = 20 * 1024 * 1024;
const LOOKUPS: usize = 512;
const PADDING_TOKEN: &str = "cursor-dispatch-harness-pad";
#[cfg(any(feature = "hotpath", feature = "hotpath-alloc"))]
const BLOCKING_LABEL: &str = "sessions.hosts.cursor.dispatch_model_blocking";
#[cfg(any(feature = "hotpath", feature = "hotpath-alloc"))]
const SCAN_LABEL: &str = "sessions.hosts.cursor.dispatch_model_scan";

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

struct Fixture {
    _tempdir: TempDir,
    parent_path: PathBuf,
    child_path: PathBuf,
    record_count: usize,
    parent_bytes: u64,
}

fn main() {
    let report = run_measured_harness();
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serialize harness report")
    );
}

#[cfg(any(feature = "hotpath", feature = "hotpath-alloc"))]
fn run_measured_harness() -> Value {
    let report_path = configure_hotpath();
    let guard = hotpath::HotpathGuardBuilder::new("cursor-dispatch-model")
        .format(hotpath::Format::Json)
        .output_path(&report_path)
        .functions_limit(512)
        .build();
    let mut report = run_harness();
    drop(guard);
    report["hotpath_report_path"] = json!(report_path.display().to_string());
    if let Ok(text) = fs::read_to_string(&report_path) {
        if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
            report["hotpath"] = extract_hotpath_labels(&parsed);
            report["hotpath_elapsed"] = parsed
                .get("elapsed_time")
                .or_else(|| parsed.get("elapsed_time_millis"))
                .cloned()
                .unwrap_or(Value::Null);
        } else {
            report["hotpath_report_parse_error"] = json!(true);
        }
    }
    report
}

#[cfg(not(any(feature = "hotpath", feature = "hotpath-alloc")))]
fn run_measured_harness() -> Value {
    run_harness()
}

#[cfg(any(feature = "hotpath", feature = "hotpath-alloc"))]
fn configure_hotpath() -> PathBuf {
    if std::env::var_os("HOTPATH_METRICS_SERVER_OFF").is_none() {
        // SAFETY: this binary is single-threaded until `main` starts the
        // workload; nothing else reads the environment concurrently.
        unsafe {
            std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1");
        }
    }
    match std::env::var_os("HOTPATH_OUTPUT_PATH") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            let path = std::env::temp_dir().join("cursor-dispatch-model-hotpath.json");
            unsafe {
                std::env::set_var("HOTPATH_OUTPUT_PATH", &path);
            }
            path
        }
    }
}

fn run_harness() -> Value {
    let fixture = write_parent_without_dispatch();
    let unchanged = measure_repeated_lookups(&fixture.child_path, LOOKUPS);
    assert!(
        unchanged.model.is_none(),
        "agent {AGENT_ID} must stay unresolved before the late dispatch"
    );

    append_late_dispatch(&fixture.parent_path);
    let after_append = lookup_once(&fixture.child_path);
    assert_eq!(
        after_append.model.as_deref(),
        Some(LATE_MODEL),
        "late-arriving dispatch must become visible on the next lookup"
    );

    let truncate = measure_truncate_rewrite();
    let inode = measure_inode_replacement();

    json!({
        "schema_version": 1,
        "workload_id": "cursor-dispatch-model-parent-rescan",
        "fixture": {
            "records": fixture.record_count,
            "parent_bytes": fixture.parent_bytes,
            "lookups": LOOKUPS,
            "agent_id": AGENT_ID,
            "parent_session_id": PARENT_SESSION_ID,
        },
        "unchanged_misses": unchanged,
        "late_dispatch": after_append,
        "truncate_rewrite": truncate,
        "inode_replacement": inode,
        "expected_uncached_parent_bytes":
            fixture.parent_bytes.saturating_mul(LOOKUPS as u64),
    })
}

#[derive(Serialize)]
struct LookupBatch {
    model: Option<String>,
    lookups: usize,
    wall_ms: u128,
    bytes_parsed: u64,
    records_parsed: u64,
    rescanned_from_zero: u64,
}

fn lookup_once(child_path: &Path) -> LookupBatch {
    measure_repeated_lookups(child_path, 1)
}

fn measure_repeated_lookups(child_path: &Path, lookups: usize) -> LookupBatch {
    let started = Instant::now();
    let mut model = None;
    let mut bytes_parsed = 0_u64;
    let mut records_parsed = 0_u64;
    let mut rescanned_from_zero = 0_u64;
    for _ in 0..lookups {
        let (found, receipt) = hotpath::measure_block!(
            "sessions.hosts.cursor.dispatch_model_blocking",
            parent_dispatch_model_for_subagent_with_receipt(
                child_path,
                PARENT_SESSION_ID,
                AGENT_ID,
            )
        );
        model = found;
        bytes_parsed = bytes_parsed.saturating_add(receipt.bytes_parsed);
        records_parsed = records_parsed.saturating_add(receipt.records_parsed);
        if receipt.rescanned_from_zero {
            rescanned_from_zero = rescanned_from_zero.saturating_add(1);
        }
    }
    LookupBatch {
        model,
        lookups,
        wall_ms: started.elapsed().as_millis(),
        bytes_parsed,
        records_parsed,
        rescanned_from_zero,
    }
}

fn write_parent_without_dispatch() -> Fixture {
    let tempdir = TempDir::new().expect("synthetic dispatch-model tempdir");
    let parent_dir = tempdir.path().join(PARENT_SESSION_ID);
    let subagents = parent_dir.join("subagents");
    fs::create_dir_all(&subagents).expect("create subagents dir");
    let parent_path = tempdir.path().join(format!("{PARENT_SESSION_ID}.jsonl"));
    let child_path = subagents.join(format!("{AGENT_ID}.jsonl"));
    fs::write(
        &child_path,
        b"{\"role\":\"assistant\",\"message\":{\"content\":\"child\"}}\n",
    )
    .expect("write child transcript");

    let padding = record_padding();
    let record = ordinary_record(&padding);
    let mut writer = BufWriter::new(File::create(&parent_path).expect("create parent transcript"));
    for _ in 0..RECORD_COUNT {
        writer
            .write_all(record.as_bytes())
            .expect("write parent record");
        writer.write_all(b"\n").expect("write parent newline");
    }
    writer.flush().expect("flush parent transcript");
    drop(writer);
    let parent_bytes = fs::metadata(&parent_path)
        .expect("stat parent transcript")
        .len();

    Fixture {
        _tempdir: tempdir,
        parent_path,
        child_path,
        record_count: RECORD_COUNT,
        parent_bytes,
    }
}

fn record_padding() -> String {
    let overhead = ordinary_record("").len() + 1;
    let per_record = (TARGET_PARENT_BYTES as usize / RECORD_COUNT).max(overhead);
    let pad_len = per_record.saturating_sub(overhead);
    let mut padding = String::with_capacity(pad_len);
    while padding.len() + PADDING_TOKEN.len() <= pad_len {
        padding.push_str(PADDING_TOKEN);
    }
    padding.push_str(&"z".repeat(pad_len.saturating_sub(padding.len())));
    padding
}

fn ordinary_record(padding: &str) -> String {
    format!(
        r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"{padding}"}}]}}}}"#
    )
}

fn dispatch_record(agent_id: &str, model: &str) -> String {
    format!(
        r#"{{"role":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu-dispatch","name":"Task","input":{{"description":"late dispatch","prompt":"resolve {agent_id}","agent_id":"{agent_id}","model":"{model}"}}}}]}}}}"#
    )
}

fn append_late_dispatch(parent_path: &Path) {
    let mut file = OpenOptions::new()
        .append(true)
        .open(parent_path)
        .expect("append parent transcript");
    writeln!(file, "{}", dispatch_record(AGENT_ID, LATE_MODEL)).expect("write late dispatch");
}

fn measure_truncate_rewrite() -> Value {
    let fixture = write_parent_without_dispatch();
    let _ = lookup_once(&fixture.child_path);
    fs::write(
        &fixture.parent_path,
        format!("{}\n", dispatch_record(AGENT_ID, "cursor-rewritten-model")),
    )
    .expect("truncate-rewrite parent");
    let after = lookup_once(&fixture.child_path);
    json!({
        "model": after.model,
        "expected_model": "cursor-rewritten-model",
        "wall_ms": after.wall_ms,
    })
}

fn measure_inode_replacement() -> Value {
    let fixture = write_parent_without_dispatch();
    let _ = lookup_once(&fixture.child_path);
    let replacement = fixture.parent_path.with_extension("jsonl.replacement");
    fs::write(
        &replacement,
        format!("{}\n", dispatch_record(AGENT_ID, "cursor-replaced-model")),
    )
    .expect("write replacement parent");
    fs::rename(&replacement, &fixture.parent_path).expect("rename over parent");
    let after = lookup_once(&fixture.child_path);
    json!({
        "model": after.model,
        "expected_model": "cursor-replaced-model",
        "wall_ms": after.wall_ms,
    })
}

#[cfg(any(feature = "hotpath", feature = "hotpath-alloc"))]
fn extract_hotpath_labels(report: &Value) -> Value {
    let mut labels = serde_json::Map::new();
    collect_labeled_metrics(report, &mut labels);
    Value::Object(labels)
}

#[cfg(any(feature = "hotpath", feature = "hotpath-alloc"))]
fn collect_labeled_metrics(value: &Value, labels: &mut serde_json::Map<String, Value>) {
    match value {
        Value::Object(map) => {
            if let Some(name) = map
                .get("name")
                .or_else(|| map.get("label"))
                .and_then(Value::as_str)
                && (name == BLOCKING_LABEL || name == SCAN_LABEL)
            {
                labels.insert(name.to_string(), Value::Object(map.clone()));
            }
            for child in map.values() {
                collect_labeled_metrics(child, labels);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_labeled_metrics(child, labels);
            }
        }
        _ => {}
    }
}
