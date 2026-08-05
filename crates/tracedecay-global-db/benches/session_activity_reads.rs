use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_store::SessionRecord;

const PROVIDER: &str = "claude";
const TARGET_SESSION: &str = "target";
const TOTAL_ROWS: i64 = 200_000;
const PAGE_SIZE: usize = 512;

fn session(session_id: &str) -> SessionRecord {
    SessionRecord {
        provider: PROVIDER.to_owned(),
        session_id: session_id.to_owned(),
        project_key: "/project".to_owned(),
        project_path: "/project".to_owned(),
        title: None,
        started_at: None,
        ended_at: None,
        transcript_path: Some(format!("/tmp/{session_id}.jsonl")),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

async fn seed_fixture(profile: &tempfile::TempDir) -> RegisteredGlobalDbTestRuntime {
    let runtime = RegisteredGlobalDbTestRuntime::profile(profile.path())
        .await
        .expect("open production registered-store fixture");
    let database = runtime.profile_database();
    for session_id in [TARGET_SESSION, "noise"] {
        assert!(database.upsert_session(&session(session_id)).await);
    }
    let transaction = database
        .begin_write_transaction()
        .await
        .expect("begin seed transaction");
    transaction
        .execute_batch(&format!(
            "WITH RECURSIVE rows(value) AS (
                 SELECT 0
                 UNION ALL
                 SELECT value + 1 FROM rows WHERE value < {}
             )
             INSERT INTO session_messages(
                 provider, message_id, session_id, role, timestamp, ordinal, text,
                 kind, model, tool_names, source_path, source_offset, metadata_json
             )
             SELECT
                 '{PROVIDER}',
                 printf('message-%06d', value),
                 CASE WHEN value % 2 = 0 THEN '{TARGET_SESSION}' ELSE 'noise' END,
                 'assistant',
                 value / 2,
                 value / 2,
                 'payload',
                 'activity',
                 NULL,
                 'tool',
                 NULL,
                 NULL,
                 NULL
             FROM rows;",
            TOTAL_ROWS - 1
        ))
        .await
        .expect("seed interleaved session activity");
    transaction.commit().await.expect("commit seed rows");
    runtime
}

fn session_activity_reads(criterion: &mut Criterion) {
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build benchmark runtime");
    let profile = tempfile::tempdir().expect("temporary benchmark profile");
    let runtime = tokio.block_on(seed_fixture(&profile));
    let database = runtime.profile_database();
    let database_path = database.db_path();
    let sibling = |suffix: &str| {
        let mut path = database_path.as_os_str().to_os_string();
        path.push(suffix);
        std::path::PathBuf::from(path)
    };
    let main_bytes = std::fs::metadata(database_path).map_or(0, |metadata| metadata.len());
    let wal_bytes = std::fs::metadata(sibling("-wal")).map_or(0, |metadata| metadata.len());
    let shm_bytes = std::fs::metadata(sibling("-shm")).map_or(0, |metadata| metadata.len());
    eprintln!(
        "fixture_storage main_db_bytes={main_bytes} wal_bytes={wal_bytes} \
         shm_bytes={shm_bytes} total_bytes={}",
        main_bytes
            .saturating_add(wal_bytes)
            .saturating_add(shm_bytes)
    );
    let cases = [
        ("current", 90_000_i64, 10_000_u64),
        ("10x", 0_i64, 100_000_u64),
    ];

    let mut group = criterion.benchmark_group("session_activity_reads");
    for (label, since_ts, eligible_rows) in cases {
        let validation = tokio
            .block_on(database.session_messages_after(
                PROVIDER,
                TARGET_SESSION,
                since_ts,
                PAGE_SIZE,
            ))
            .expect("validate production activity read");
        assert_eq!(validation.len(), PAGE_SIZE);
        assert!(validation.windows(2).all(|rows| {
            (rows[0].timestamp, rows[0].ordinal) <= (rows[1].timestamp, rows[1].ordinal)
        }));

        group.throughput(Throughput::Elements(eligible_rows));
        group.bench_with_input(
            BenchmarkId::new("eligible_rows", label),
            &since_ts,
            |bencher, since_ts| {
                bencher.to_async(&tokio).iter(|| async {
                    let rows = database
                        .session_messages_after(PROVIDER, TARGET_SESSION, *since_ts, PAGE_SIZE)
                        .await
                        .expect("query production session activity");
                    criterion::black_box(rows)
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, session_activity_reads);
criterion_main!(benches);
