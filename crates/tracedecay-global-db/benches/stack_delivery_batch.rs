use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tracedecay_global_db::{
    GitHubStackDeliveryKeyV1, GitHubStackSignalRecordV1,
    tests::harness::RegisteredGlobalDbTestRuntime,
};

const PROJECT_ID: &str = "project.stack-delivery-benchmark";
const WATERMARK_ID: &str = "watermark.stack-delivery-benchmark";

async fn seed_batch(
    database: &tracedecay_global_db::RegisteredGlobalDb,
    batch_size: usize,
) -> Vec<GitHubStackDeliveryKeyV1> {
    let signal_id = format!("signal.stack-delivery-benchmark.{batch_size}");
    let recipients = (0..batch_size)
        .map(|index| format!("actor.stack-delivery-benchmark.{batch_size}.{index}"))
        .collect::<Vec<_>>();
    database
        .append_github_stack_signal(
            GitHubStackSignalRecordV1 {
                project_id: PROJECT_ID.to_owned(),
                signal_id,
                scope_digest: "sha256:stack-delivery-benchmark".to_owned(),
                repository_id: "repository.stack-delivery-benchmark".to_owned(),
                watermark_id: WATERMARK_ID.to_owned(),
                observed_at_micros: i64::try_from(batch_size).unwrap() + 1,
                signal_json: "{}".to_owned(),
            },
            recipients,
        )
        .await
        .unwrap();
    let deliveries = database
        .pending_github_stack_deliveries(PROJECT_ID, batch_size)
        .await
        .unwrap();
    let keys = deliveries
        .iter()
        .map(GitHubStackDeliveryKeyV1::from)
        .collect::<Vec<_>>();
    assert_eq!(keys.len(), batch_size);
    database
        .publish_github_stack_deliveries(PROJECT_ID, WATERMARK_ID, &keys)
        .await
        .unwrap();
    keys
}

fn stack_delivery_batch(criterion: &mut Criterion) {
    let executor = asupersync::runtime::RuntimeBuilder::current_thread()
        .blocking_threads(0, 512)
        .build()
        .unwrap();
    let profile = tempfile::tempdir().unwrap();
    let runtime = executor
        .block_on(RegisteredGlobalDbTestRuntime::profile(profile.path()))
        .unwrap();
    let database = runtime.profile_database();
    let cases = [1_usize, 64, 256]
        .into_iter()
        .map(|batch_size| {
            (
                batch_size,
                executor.block_on(seed_batch(database, batch_size)),
            )
        })
        .collect::<Vec<_>>();

    let mut group = criterion.benchmark_group("stack_delivery_host_pending_replay");
    for (batch_size, keys) in &cases {
        group.throughput(Throughput::Elements(*batch_size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            keys,
            |bencher, keys| {
                bencher.to_async(BenchExecutor(&executor)).iter(|| async {
                    database
                        .publish_github_stack_deliveries(PROJECT_ID, WATERMARK_ID, keys)
                        .await
                        .unwrap()
                });
            },
        );
    }
    group.finish();
}

struct BenchExecutor<'a>(&'a asupersync::runtime::Runtime);

impl criterion::async_executor::AsyncExecutor for BenchExecutor<'_> {
    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        self.0.block_on(future)
    }
}

criterion_group!(benches, stack_delivery_batch);
criterion_main!(benches);
