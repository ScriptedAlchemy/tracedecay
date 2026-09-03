//! Opt-in large authority-convergence capture for issue-scale comparisons.
//!
//! Run with:
//! `source scripts/hotpath-rustflags.sh`
//! `cargo test -p tracedecay-global-db --test schema_convergence_hotpath
//!    --features test-helpers,hotpath-alloc -- --ignored --nocapture`
//!
//! SQLite is bundled and allocates through libc, so Hotpath's counting
//! allocator reports Rust-side allocations only.

use std::time::Instant;

use serde_json::Value;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, ComponentVersion, DurableObservationV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
};
use tracedecay_global_db::schema_stages::RegisteredSchemaConvergence;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_runtime_core::db::engine::params;

#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

const SHARD_COUNT: usize = 4;
const ROWS_PER_SHARD: usize = 97;

fn authority_fixture(
    shard: usize,
    index: usize,
) -> (DurableObservationV1, ObservationSourceCursorV1) {
    let record_id = format!("record.convergence-{shard}-{index}");
    let session_id = format!("convergence-{shard}-{index}");
    let mut fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/provider_normalization/codex/session_meta.expected_envelope.json"
    ))
    .expect("checked-in codex envelope fixture");
    fixture["stable_record_id"] = Value::String(record_id.clone());
    fixture["relations"]["session_id"] = Value::String(session_id.clone());
    fixture["relations"]["thread_id"] = Value::String(session_id);
    let envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(fixture).expect("decode codex envelope fixture");
    let source = ObservationSourceIdentityV1::for_provider(
        envelope.provider().clone(),
        envelope.relations().session_id().clone(),
    )
    .expect("observation source identity");
    let payload = serde_json::to_value(envelope).expect("encode codex envelope fixture");
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.convergence-{shard}-{index}"))
                .expect("receipt identity"),
            ComponentVersion::new("sanitizer.convergence.v1").expect("sanitizer version"),
        )
        .expect("receipt reference"),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).expect("payload reference")),
    )
    .expect("sanitization receipt");
    let generation = ObservationSourceGenerationV1::new(1).expect("source generation");
    let start = u64::try_from(index).expect("fixture index") * 100;
    let end = start + 100;
    let observation = DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source.clone(),
            ObservationScopeV1::Profile,
            generation,
            ObservationSourceRangeV1::new(start, end).expect("source range"),
            ObservationOrderingDomainV1::FileBytes,
            ObservationId::new(record_id).expect("observation identity"),
        )
        .expect("observation identity material"),
        receipt,
        RetentionClass::new("retention.convergence").expect("retention class"),
        payload,
    )
    .expect("durable observation");
    let cursor =
        ObservationSourceCursorV1::new(source, ObservationScopeV1::Profile, generation, end)
            .expect("committed source cursor");
    (observation, cursor)
}

async fn seed_authority_rows(runtime: &RegisteredGlobalDbTestRuntime, shard: usize) {
    let transaction = runtime
        .profile_database()
        .begin_write_transaction()
        .await
        .expect("begin authority fixture transaction");
    for index in 0..ROWS_PER_SHARD {
        let (observation, cursor) = authority_fixture(shard, index);
        let receipt = observation.receipt();
        transaction
            .execute(
                "INSERT INTO sanitization_receipts
                 (receipt_id, sanitizer_version, payload_digest, receipt_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    receipt.receipt().receipt_id().as_str(),
                    receipt.receipt().sanitizer_version().as_str(),
                    observation.payload_reference().digest().as_str(),
                    serde_json::to_string(receipt).expect("encode receipt")
                ],
            )
            .await
            .expect("seed sanitization receipt");
        transaction
            .execute(
                "INSERT INTO observations
                 (observation_id, payload_digest, receipt_id, observation_json,
                  committed_cursor_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    observation.observation_id().as_str(),
                    observation.payload_reference().digest().as_str(),
                    receipt.receipt().receipt_id().as_str(),
                    serde_json::to_string(&observation).expect("encode observation"),
                    serde_json::to_string(&cursor).expect("encode cursor")
                ],
            )
            .await
            .expect("seed committed observation");
        transaction
            .execute(
                "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
                 VALUES (?1, ?2, ?3)",
                params![
                    serde_json::to_string(cursor.source()).expect("encode source"),
                    serde_json::to_string(cursor.scope()).expect("encode scope"),
                    serde_json::to_string(&cursor).expect("encode source cursor")
                ],
            )
            .await
            .expect("seed source cursor");
    }
    transaction
        .execute("DELETE FROM authority_audit_checkpoints", ())
        .await
        .expect("arm exhaustive convergence");
    transaction
        .commit()
        .await
        .expect("commit authority fixture");
}

fn proc_value(path: &str, key: &str) -> u64 {
    std::fs::read_to_string(path)
        .expect("read Linux process counters")
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .expect("process counter is available")
}

fn report_entries<'a>(report: &'a Value, section: &str) -> &'a [Value] {
    report[section]["data"]
        .as_array()
        .map(Vec::as_slice)
        .expect("Hotpath report section data")
}

fn matching_entry<'a>(entries: &'a [Value], label: &str) -> Option<&'a Value> {
    entries.iter().find(|entry| {
        entry["name"].as_str() == Some(label) || entry["label"].as_str() == Some(label)
    })
}

fn print_convergence_report(report: &Value) {
    for entry in report_entries(report, "functions_alloc")
        .iter()
        .filter(|entry| {
            entry["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("global_db.schema.persist.converge"))
        })
    {
        println!(
            "function_alloc label={} calls={} exclusive_total={}",
            entry["name"].as_str().expect("allocation label"),
            entry["calls"].as_u64().expect("allocation calls"),
            entry["total"].as_str().expect("exclusive allocation total")
        );
    }
    for entry in report_entries(report, "futures").iter().filter(|entry| {
        entry["label"]
            .as_str()
            .is_some_and(|label| label.starts_with("global_db.schema.persist.converge"))
    }) {
        println!(
            "future_alloc label={} calls={} poll_bytes={} poll_allocations={}",
            entry["label"].as_str().expect("future label"),
            entry["call_count"].as_u64().expect("future calls"),
            entry["total_poll_alloc_bytes"]
                .as_u64()
                .expect("future allocation bytes"),
            entry["total_poll_alloc_count"]
                .as_u64()
                .expect("future allocation count")
        );
    }
}

#[test]
#[ignore = "matched large-store Hotpath capture"]
fn profile_large_registered_schema_convergence() {
    // This binary contains one test, so its environment is process-exclusive.
    unsafe {
        std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1");
        std::env::set_var("HOTPATH_REPORT", "functions-timing,functions-alloc,futures");
    }
    let report_directory = tempfile::tempdir().expect("Hotpath report directory");
    let report_path = report_directory.path().join("schema-convergence.json");
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("schema convergence runtime");

    let runtimes = tokio.block_on(async {
        let mut runtimes = Vec::with_capacity(SHARD_COUNT);
        for shard in 0..SHARD_COUNT {
            let profile = tempfile::tempdir().expect("temporary shard profile");
            let runtime = RegisteredGlobalDbTestRuntime::profile(profile.keep())
                .await
                .expect("open registered shard");
            seed_authority_rows(&runtime, shard).await;
            runtimes.push(runtime);
        }
        runtimes
    });

    let guard = hotpath::HotpathGuardBuilder::new("global-db-schema-convergence")
        .format(hotpath::Format::Json)
        .output_path(&report_path)
        .functions_limit(1024)
        .percentiles(&[95.0, 100.0])
        .build();
    let read_bytes_before = proc_value("/proc/self/io", "read_bytes:");
    let hwm_before_kib = proc_value("/proc/self/status", "VmHWM:");
    let started = Instant::now();
    tokio.block_on(async {
        for runtime in &runtimes {
            runtime
                .profile_database()
                .converge_schema(RegisteredSchemaConvergence::exhaustive_for_test())
                .await
                .expect("converge registered shard");
        }
    });
    let elapsed = started.elapsed();
    let read_bytes = proc_value("/proc/self/io", "read_bytes:").saturating_sub(read_bytes_before);
    let hwm_after_kib = proc_value("/proc/self/status", "VmHWM:");
    drop(guard);

    let report: Value = serde_json::from_str(
        &std::fs::read_to_string(&report_path).expect("read Hotpath convergence report"),
    )
    .expect("decode Hotpath convergence report");
    print_convergence_report(&report);
    let outer = matching_entry(
        report_entries(&report, "functions_timing"),
        "global_db.schema.persist.converge",
    )
    .expect("outer convergence timing");
    let step = matching_entry(
        report_entries(&report, "functions_timing"),
        "global_db.schema.persist.converge_step",
    )
    .expect("convergence step timing");
    println!("shards={SHARD_COUNT}");
    println!("rows_per_shard={ROWS_PER_SHARD}");
    println!("elapsed_ms={}", elapsed.as_millis());
    println!("vmhwm_kib={hwm_after_kib}");
    println!(
        "vmhwm_delta_kib={}",
        hwm_after_kib.saturating_sub(hwm_before_kib)
    );
    println!("read_bytes={read_bytes}");
    println!(
        "write_transactions={}",
        step["calls"].as_u64().expect("convergence step calls")
    );
    println!(
        "max_writer_lane_hold={}",
        step["p100"].as_str().expect("convergence step p100")
    );
    println!(
        "outer_p100={}",
        outer["p100"].as_str().expect("outer p100 duration")
    );
    println!("allocator_scope=rust-only; bundled SQLite libc allocations are invisible");
}
