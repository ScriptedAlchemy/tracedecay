//! Controlled, machine-readable acceptance workloads for capture composition
//! and private-fs framed-log durability.
//!
//! This crate does not declare a `hotpath` feature. Workloads measure wall
//! time and byte identity so a composing binary can compare capture/private-fs
//! feature-off vs feature-on runs without production-only annotations.
//! Acceptance is durable identity (`bytes_match` / `both_ok`), not latency.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_capture::{cursor, cursor_composer, parse_normalized_observation_record_v1};
use tracedecay_domain::{ClaudeByteRangeV1, ObservationOrderingDomainV1};
use tracedecay_private_fs::framed_log::{
    DirectorySyncPolicy, append_durable, atomic_write, read_bounded, sync_directory, truncate_file,
};

const SCHEMA_VERSION: u32 = 1;
pub const FRAMED_LOG_WORKLOAD: &str = "framed_log_durability";
pub const CURSOR_PARSE_WORKLOAD: &str = "cursor_parse_batch";
pub const FRAMED_LOG_REPORT_FILE: &str = "framed_log_durability.json";
pub const CURSOR_PARSE_REPORT_FILE: &str = "cursor_parse_batch.json";
const FRAME_BYTES: usize = 32 * 1024;
const PUBLISH_BYTES: usize = 64 * 1024;
const APPEND_FRAMES: u64 = 8;
const PARSE_RECORDS: u64 = 24;
const DIRECTORY_POLICY: DirectorySyncPolicy = DirectorySyncPolicy::TolerateUnsupported;

#[derive(Debug, Error)]
pub enum ControlledWorkloadErrorV1 {
    #[error("{0}")]
    Contract(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlledOperationV1 {
    pub operation: String,
    pub iterations: u64,
    pub bytes: u64,
    pub elapsed_us: u64,
    pub verify_digest: Option<String>,
    pub ok: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlledWorkloadReportV1 {
    pub schema_version: u32,
    pub workload: String,
    pub operations: Vec<ControlledOperationV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlledOperationDeltaV1 {
    pub operation: String,
    pub left_elapsed_us: Option<u64>,
    pub right_elapsed_us: Option<u64>,
    pub elapsed_delta_us: Option<i64>,
    pub bytes_match: bool,
    pub both_ok: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlledWorkloadComparisonV1 {
    pub schema_version: u32,
    pub left_workload: String,
    pub right_workload: String,
    pub operations: Vec<ControlledOperationDeltaV1>,
}

pub fn compare_controlled_workloads(
    left: &ControlledWorkloadReportV1,
    right: &ControlledWorkloadReportV1,
) -> Result<ControlledWorkloadComparisonV1, ControlledWorkloadErrorV1> {
    if left.schema_version != SCHEMA_VERSION || right.schema_version != SCHEMA_VERSION {
        return Err(ControlledWorkloadErrorV1::Contract(
            "controlled workload schema_version must be 1".to_owned(),
        ));
    }
    let left_ops = operations_by_name(left)?;
    let right_ops = operations_by_name(right)?;
    let names = left_ops
        .keys()
        .chain(right_ops.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let operations = names
        .into_iter()
        .map(|operation| {
            let left_op = left_ops.get(&operation);
            let right_op = right_ops.get(&operation);
            let elapsed_delta_us = match (left_op, right_op) {
                (Some(left_op), Some(right_op)) => {
                    Some(signed_delta(left_op.elapsed_us, right_op.elapsed_us))
                }
                _ => None,
            };
            let bytes_match = match (left_op, right_op) {
                (Some(left_op), Some(right_op)) => {
                    left_op.bytes == right_op.bytes
                        && left_op.verify_digest == right_op.verify_digest
                }
                _ => false,
            };
            ControlledOperationDeltaV1 {
                operation,
                left_elapsed_us: left_op.map(|operation| operation.elapsed_us),
                right_elapsed_us: right_op.map(|operation| operation.elapsed_us),
                elapsed_delta_us,
                bytes_match,
                both_ok: left_op.is_some_and(|operation| operation.ok)
                    && right_op.is_some_and(|operation| operation.ok),
            }
        })
        .collect();
    Ok(ControlledWorkloadComparisonV1 {
        schema_version: SCHEMA_VERSION,
        left_workload: left.workload.clone(),
        right_workload: right.workload.clone(),
        operations,
    })
}

impl ControlledWorkloadComparisonV1 {
    /// Durable identity only: byte counts, verify digests, and success flags.
    /// Timing fields are informational and must not be used as acceptance.
    pub fn durable_results_identical(&self) -> bool {
        !self.operations.is_empty()
            && self
                .operations
                .iter()
                .all(|operation| operation.bytes_match && operation.both_ok)
    }
}

pub fn write_controlled_workload_reports(
    report_dir: &Path,
) -> Result<(), ControlledWorkloadErrorV1> {
    fs::create_dir_all(report_dir)?;
    let framed_root = report_dir.join("framed_log_root");
    fs::create_dir_all(&framed_root)?;
    let framed = run_framed_log_durability_workload(&framed_root)?;
    let cursor = run_cursor_parse_batch_workload()?;
    write_report_json(&report_dir.join(FRAMED_LOG_REPORT_FILE), &framed)?;
    write_report_json(&report_dir.join(CURSOR_PARSE_REPORT_FILE), &cursor)?;
    Ok(())
}

pub fn run_framed_log_durability_workload(
    root: &Path,
) -> Result<ControlledWorkloadReportV1, ControlledWorkloadErrorV1> {
    let append_path = root.join("frames.bin");
    let publish_path = root.join("published.bin");
    let first_frame = repeating_frame(0xA1);
    let later_frame = repeating_frame(0xB2);
    let published = vec![0xC3; PUBLISH_BYTES];

    let append = timed("framed_log.append_durable", APPEND_FRAMES, || {
        append_durable(&append_path, &first_frame, DIRECTORY_POLICY)?;
        for _ in 1..APPEND_FRAMES {
            append_durable(&append_path, &later_frame, DIRECTORY_POLICY)?;
        }
        Ok((
            first_frame.len() as u64 * APPEND_FRAMES,
            Some(digest_hex(&first_frame)),
        ))
    })?;

    let publish = timed("framed_log.atomic_write", 1, || {
        atomic_write(&publish_path, "eval", &published, DIRECTORY_POLICY)?;
        Ok((published.len() as u64, Some(digest_hex(&published))))
    })?;

    let verify = timed("framed_log.read_bounded_verify", 1, || {
        let bytes = read_bounded(&publish_path, PUBLISH_BYTES)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "published framed-log bytes are missing",
            )
        })?;
        if bytes != published {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "published framed-log bytes do not match",
            ));
        }
        Ok((bytes.len() as u64, Some(digest_hex(&bytes))))
    })?;

    let recover = timed("framed_log.truncate_recovery", 1, || {
        truncate_file(&append_path, first_frame.len() as u64, DIRECTORY_POLICY)?;
        let bytes = read_bounded(&append_path, FRAME_BYTES)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "recovered framed-log bytes are missing",
            )
        })?;
        if bytes != first_frame {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "recovered framed-log prefix is not the first frame",
            ));
        }
        Ok((bytes.len() as u64, Some(digest_hex(&bytes))))
    })?;

    let fsync = timed("framed_log.sync_directory", 1, || {
        sync_directory(root, DIRECTORY_POLICY)?;
        Ok((0, None))
    })?;

    Ok(ControlledWorkloadReportV1 {
        schema_version: SCHEMA_VERSION,
        workload: FRAMED_LOG_WORKLOAD.to_owned(),
        operations: vec![append, publish, verify, recover, fsync],
    })
}

pub fn run_cursor_parse_batch_workload()
-> Result<ControlledWorkloadReportV1, ControlledWorkloadErrorV1> {
    let cursor_records = cursor_batch_records(PARSE_RECORDS)?;
    let composer_records = composer_batch_records(PARSE_RECORDS)?;
    let cursor = timed("capture.cursor_parse_batch", PARSE_RECORDS, || {
        compose_cursor_batch(&cursor_records)
    })?;
    let composer = timed("capture.cursor_composer_parse_batch", PARSE_RECORDS, || {
        compose_composer_batch(&composer_records)
    })?;
    Ok(ControlledWorkloadReportV1 {
        schema_version: SCHEMA_VERSION,
        workload: CURSOR_PARSE_WORKLOAD.to_owned(),
        operations: vec![cursor, composer],
    })
}

fn compose_cursor_batch(records: &[Vec<u8>]) -> std::io::Result<(u64, Option<String>)> {
    let mut offset = 0_u64;
    let mut digests = Sha256::new();
    let mut bytes = 0_u64;
    for record in records {
        let end = offset.saturating_add(record.len() as u64);
        let range = ClaudeByteRangeV1::new(offset, end).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
        })?;
        let parsed = parse_normalized_observation_record_v1(
            record,
            range,
            ObservationOrderingDomainV1::FileBytes,
            |native| {
                let record_id =
                    cursor::observation_native_record_id("cursor", "cursor-eval-session", &native)?;
                cursor::normalize_cursor_observation(
                    &native,
                    "cursor-eval-session",
                    record_id,
                    range,
                    None,
                    None,
                )
            },
        )
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))?;
        digests.update(parsed.raw_digest());
        bytes = bytes.saturating_add(record.len() as u64);
        offset = end;
    }
    Ok((bytes, Some(hex::encode(digests.finalize()))))
}

fn compose_composer_batch(records: &[Vec<u8>]) -> std::io::Result<(u64, Option<String>)> {
    let mut offset = 0_u64;
    let mut digests = Sha256::new();
    let mut bytes = 0_u64;
    for (index, record) in records.iter().enumerate() {
        let end = offset.saturating_add(record.len() as u64);
        let range = ClaudeByteRangeV1::new(offset, end).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
        })?;
        let position = index as u64 + 1;
        let parsed = parse_normalized_observation_record_v1(
            record,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            |native| {
                let record_id = cursor_composer::cursor_composer_native_record_id(
                    "comp-eval",
                    &format!("b-{position}"),
                )
                .map_err(|_| {
                    tracedecay_capture::ObservationRecordParseErrorV1::NormalizationFailed
                })?;
                cursor_composer::normalize_cursor_composer_observation(
                    &native,
                    "comp-eval",
                    record_id,
                    range,
                    position,
                )
            },
        )
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))?;
        digests.update(parsed.raw_digest());
        bytes = bytes.saturating_add(record.len() as u64);
        offset = end;
    }
    Ok((bytes, Some(hex::encode(digests.finalize()))))
}

fn cursor_batch_records(count: u64) -> std::io::Result<Vec<Vec<u8>>> {
    (0..count)
        .map(|index| {
            serde_json::to_vec(&serde_json::json!({
                "type": "assistant",
                "role": "assistant",
                "model": "gpt-5.2",
                "message": { "content": format!("Refactored module {index}.") },
                "tokenCount": { "inputTokens": 80 + index, "outputTokens": 12 }
            }))
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
        .collect()
}

fn composer_batch_records(count: u64) -> std::io::Result<Vec<Vec<u8>>> {
    (0..count)
        .map(|index| {
            serde_json::to_vec(&serde_json::json!({
                "type": 2,
                "text": format!("Done refactoring module {index}."),
                "tokenCount": { "inputTokens": 40 + index, "outputTokens": 8 }
            }))
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
        .collect()
}

fn timed(
    operation: &str,
    iterations: u64,
    run: impl FnOnce() -> std::io::Result<(u64, Option<String>)>,
) -> Result<ControlledOperationV1, ControlledWorkloadErrorV1> {
    let started = Instant::now();
    match run() {
        Ok((bytes, verify_digest)) => Ok(ControlledOperationV1 {
            operation: operation.to_owned(),
            iterations,
            bytes,
            elapsed_us: elapsed_us(started),
            verify_digest,
            ok: true,
        }),
        Err(error) => Err(ControlledWorkloadErrorV1::Io(error)),
    }
}

fn repeating_frame(fill: u8) -> Vec<u8> {
    vec![fill; FRAME_BYTES]
}

fn digest_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn elapsed_us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn signed_delta(left: u64, right: u64) -> i64 {
    match (i64::try_from(left), i64::try_from(right)) {
        (Ok(left), Ok(right)) => right.saturating_sub(left),
        _ => 0,
    }
}

fn write_report_json(
    path: &Path,
    report: &ControlledWorkloadReportV1,
) -> Result<(), ControlledWorkloadErrorV1> {
    let bytes = serde_json::to_vec(report).map_err(|error| {
        ControlledWorkloadErrorV1::Contract(format!(
            "failed to encode controlled workload report: {error}"
        ))
    })?;
    fs::write(path, bytes)?;
    Ok(())
}

fn operations_by_name(
    report: &ControlledWorkloadReportV1,
) -> Result<BTreeMap<String, &ControlledOperationV1>, ControlledWorkloadErrorV1> {
    let mut operations = BTreeMap::new();
    for operation in &report.operations {
        if operations
            .insert(operation.operation.clone(), operation)
            .is_some()
        {
            return Err(ControlledWorkloadErrorV1::Contract(format!(
                "duplicate controlled operation {}",
                operation.operation
            )));
        }
    }
    Ok(operations)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{
        CURSOR_PARSE_REPORT_FILE, CURSOR_PARSE_WORKLOAD, ControlledOperationV1,
        ControlledWorkloadReportV1, FRAMED_LOG_REPORT_FILE, FRAMED_LOG_WORKLOAD, PARSE_RECORDS,
        compare_controlled_workloads, run_cursor_parse_batch_workload,
        run_framed_log_durability_workload, write_controlled_workload_reports,
    };

    const BUILD_IDENTITY_FILE: &str = "controlled-workload-build-identity.json";
    const HOTPATH_OFF_BIN_ENV: &str = "TRACEDECAY_CONTROLLED_WORKLOAD_HOTPATH_OFF_BIN";
    const HOTPATH_ON_BIN_ENV: &str = "TRACEDECAY_CONTROLLED_WORKLOAD_HOTPATH_ON_BIN";

    #[test]
    fn framed_log_workload_recovers_the_first_frame_and_verifies_publish() {
        let root = tempfile::tempdir().expect("framed-log workload root");
        let report = run_framed_log_durability_workload(root.path()).expect("framed-log workload");

        assert_eq!(report.workload, FRAMED_LOG_WORKLOAD);
        assert_eq!(report.schema_version, 1);
        assert_eq!(report.operations.len(), 5);
        assert!(
            report.operations.iter().all(|operation| operation.ok),
            "every framed-log operation must succeed: {report:?}"
        );
        let recover = report
            .operations
            .iter()
            .find(|operation| operation.operation == "framed_log.truncate_recovery")
            .expect("recovery sample");
        let verify = report
            .operations
            .iter()
            .find(|operation| operation.operation == "framed_log.read_bounded_verify")
            .expect("verify sample");
        assert!(recover.verify_digest.is_some());
        assert!(verify.verify_digest.is_some());
        assert_ne!(recover.verify_digest, verify.verify_digest);
    }

    #[test]
    fn cursor_parse_batch_composes_one_envelope_per_source_record() {
        let report = run_cursor_parse_batch_workload().expect("cursor parse batch");

        assert_eq!(report.workload, CURSOR_PARSE_WORKLOAD);
        assert_eq!(report.operations.len(), 2);
        for operation in &report.operations {
            assert!(operation.ok, "{operation:?}");
            assert_eq!(operation.iterations, PARSE_RECORDS);
            assert!(operation.bytes > 0);
            assert!(operation.verify_digest.is_some());
        }
    }

    #[test]
    fn workload_comparison_is_machine_readable_and_byte_stable() {
        let left_root = tempfile::tempdir().expect("left comparison root");
        let right_root = tempfile::tempdir().expect("right comparison root");
        let left = run_framed_log_durability_workload(left_root.path()).expect("left");
        let right = run_framed_log_durability_workload(right_root.path()).expect("right");
        let comparison = compare_controlled_workloads(&left, &right).expect("compare");

        assert_eq!(comparison.schema_version, 1);
        assert_eq!(comparison.operations.len(), left.operations.len());
        assert!(
            comparison.durable_results_identical(),
            "repeat runs must keep byte identity: {comparison:?}"
        );
        let encoded = serde_json::to_value(&comparison).expect("comparison json");
        assert_eq!(encoded["schema_version"], 1);
        assert!(
            encoded["operations"]
                .as_array()
                .is_some_and(|ops| !ops.is_empty())
        );

        let cursor_left = run_cursor_parse_batch_workload().expect("cursor left");
        let cursor_right = run_cursor_parse_batch_workload().expect("cursor right");
        let cursor_comparison =
            compare_controlled_workloads(&cursor_left, &cursor_right).expect("compare cursor");
        assert!(
            cursor_comparison.durable_results_identical(),
            "repeat cursor-parse runs must keep byte identity: {cursor_comparison:?}"
        );
    }

    #[test]
    fn durable_identity_ignores_elapsed_mismatch() {
        let left = report(
            FRAMED_LOG_WORKLOAD,
            vec![operation("framed_log.append_durable", 32, "abc", 10, true)],
        );
        let right = report(
            FRAMED_LOG_WORKLOAD,
            vec![operation(
                "framed_log.append_durable",
                32,
                "abc",
                9_999,
                true,
            )],
        );
        let comparison = compare_controlled_workloads(&left, &right).expect("compare");

        assert_ne!(
            comparison.operations[0].left_elapsed_us,
            comparison.operations[0].right_elapsed_us
        );
        assert!(comparison.operations[0].bytes_match);
        assert!(comparison.operations[0].both_ok);
        assert!(
            comparison.durable_results_identical(),
            "elapsed deltas are not acceptance: {comparison:?}"
        );
    }

    #[test]
    fn durable_identity_rejects_mismatched_verify_digests() {
        let left = report(
            CURSOR_PARSE_WORKLOAD,
            vec![operation("capture.cursor_parse_batch", 16, "aaa", 1, true)],
        );
        let right = report(
            CURSOR_PARSE_WORKLOAD,
            vec![operation("capture.cursor_parse_batch", 16, "bbb", 1, true)],
        );
        let comparison = compare_controlled_workloads(&left, &right).expect("compare");

        assert!(!comparison.operations[0].bytes_match);
        assert!(comparison.operations[0].both_ok);
        assert!(!comparison.durable_results_identical());
    }

    #[test]
    fn durable_identity_rejects_empty_operations() {
        let left = report(FRAMED_LOG_WORKLOAD, Vec::new());
        let right = report(FRAMED_LOG_WORKLOAD, Vec::new());
        let comparison = compare_controlled_workloads(&left, &right).expect("compare");

        assert!(comparison.operations.is_empty());
        assert!(!comparison.durable_results_identical());
    }

    #[test]
    fn write_reports_cover_framed_log_and_cursor_parse() {
        let dir = tempfile::tempdir().expect("report dir");
        write_controlled_workload_reports(dir.path()).expect("write reports");

        let framed = load_report(&dir.path().join(FRAMED_LOG_REPORT_FILE));
        let cursor = load_report(&dir.path().join(CURSOR_PARSE_REPORT_FILE));
        assert_eq!(framed.workload, FRAMED_LOG_WORKLOAD);
        assert_eq!(cursor.workload, CURSOR_PARSE_WORKLOAD);
        assert!(!framed.operations.is_empty());
        assert!(!cursor.operations.is_empty());
        assert!(framed.operations.iter().all(|operation| operation.ok));
        assert!(cursor.operations.iter().all(|operation| operation.ok));
    }

    // The parity executables are a second, feature-on build of this crate's
    // dependency graph, so no ordinary test run can produce them. The workflow
    // helper builds the pair and a dedicated `--run-ignored only` step runs
    // this test against it; the Windows archive job cannot afford that rebuild
    // inside its bound (it spent 18 of its 62 minutes on it) and the property
    // is platform-independent, so only the Linux lane provisions and runs it.
    #[test]
    #[ignore = "requires feature-off/on executables built by the workflow helper"]
    fn hotpath_off_vs_on_durable_results_are_identical() {
        let scratch = tempfile::tempdir().expect("identity scratch");
        let off_dir = scratch.path().join("off");
        let on_dir = scratch.path().join("on");
        let (off_executable, on_executable) = controlled_workload_executable_pair();

        emit_reports(&off_executable, &off_dir);
        emit_reports(&on_executable, &on_dir);

        let off_build = load_build_identity(&off_dir);
        let on_build = load_build_identity(&on_dir);
        assert_eq!(off_build["feature_mode"], "hotpath-off");
        assert_eq!(on_build["feature_mode"], "hotpath-on");
        assert_ne!(
            off_build["executable_sha256"], on_build["executable_sha256"],
            "feature-off and feature-on reports must come from distinct executable bytes"
        );

        assert_durable_identity(
            FRAMED_LOG_WORKLOAD,
            &load_report(&off_dir.join(FRAMED_LOG_REPORT_FILE)),
            &load_report(&on_dir.join(FRAMED_LOG_REPORT_FILE)),
        );
        assert_durable_identity(
            CURSOR_PARSE_WORKLOAD,
            &load_report(&off_dir.join(CURSOR_PARSE_REPORT_FILE)),
            &load_report(&on_dir.join(CURSOR_PARSE_REPORT_FILE)),
        );
    }

    fn report(
        workload: &str,
        operations: Vec<ControlledOperationV1>,
    ) -> ControlledWorkloadReportV1 {
        ControlledWorkloadReportV1 {
            schema_version: 1,
            workload: workload.to_owned(),
            operations,
        }
    }

    fn operation(
        name: &str,
        bytes: u64,
        digest: &str,
        elapsed_us: u64,
        ok: bool,
    ) -> ControlledOperationV1 {
        ControlledOperationV1 {
            operation: name.to_owned(),
            iterations: 1,
            bytes,
            elapsed_us,
            verify_digest: Some(digest.to_owned()),
            ok,
        }
    }

    fn load_report(path: &Path) -> ControlledWorkloadReportV1 {
        let bytes =
            std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
    }

    fn load_build_identity(report_dir: &Path) -> serde_json::Value {
        let path = report_dir.join(BUILD_IDENTITY_FILE);
        let bytes =
            std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
    }

    fn assert_durable_identity(
        workload: &str,
        off: &ControlledWorkloadReportV1,
        on: &ControlledWorkloadReportV1,
    ) {
        assert_eq!(off.workload, workload);
        assert_eq!(on.workload, workload);
        let comparison = compare_controlled_workloads(off, on).expect("compare");
        assert!(
            !comparison.operations.is_empty(),
            "{workload} comparison must not be empty"
        );
        assert!(
            comparison
                .operations
                .iter()
                .all(|operation| operation.bytes_match && operation.both_ok),
            "{workload} feature-off vs feature-on durable results must match: {comparison:?}"
        );
        assert!(comparison.durable_results_identical());
    }

    fn controlled_workload_executable_pair() -> (PathBuf, PathBuf) {
        let configured_off = std::env::var_os(HOTPATH_OFF_BIN_ENV).map(PathBuf::from);
        let configured_on = std::env::var_os(HOTPATH_ON_BIN_ENV).map(PathBuf::from);
        let pair = match (configured_off, configured_on) {
            (Some(off), Some(on)) => (off, on),
            (None, None) => {
                let current_executable = std::env::current_exe().expect("current test executable");
                let target_root = current_executable
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::parent)
                    .expect("test executable must be under target/<profile>/deps");
                let helpers = target_root.join("controlled-workload-hotpath");
                (
                    helpers.join(format!("hotpath-off{}", std::env::consts::EXE_SUFFIX)),
                    helpers.join(format!("hotpath-on{}", std::env::consts::EXE_SUFFIX)),
                )
            }
            _ => {
                panic!("{HOTPATH_OFF_BIN_ENV} and {HOTPATH_ON_BIN_ENV} must be configured together")
            }
        };
        for executable in [&pair.0, &pair.1] {
            assert!(
                executable.is_file(),
                "controlled-workload parity executable is missing at {}; prebuild both feature modes before running tests",
                executable.display()
            );
        }
        pair
    }

    fn emit_reports(executable: &Path, report_dir: &Path) {
        std::fs::create_dir_all(report_dir).expect("report dir");
        let output = Command::new(executable)
            .arg(report_dir)
            .output()
            .unwrap_or_else(|error| panic!("spawn {}: {error}", executable.display()));
        assert!(
            output.status.success(),
            "controlled-workload helper {} failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
            executable.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
