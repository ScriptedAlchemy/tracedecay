//! Measures how per-generation publication cost scales with the size of the
//! accumulated staging store.
//!
//! The real code graph publishes one generation per shard projection into a
//! single Grafeo store, so by the time shard N publishes, the store already
//! holds shards 1..N. If any publication phase does work proportional to the
//! whole store rather than the generation's own rows, per-publish wall time
//! grows with the shard index and the total build cost is quadratic.
//!
//! Run (wall times only):
//!   cargo bench -p tracedecay-graph-db --bench publication_scale \
//!     --features test-helpers
//! Run with phase attribution:
//!   cargo bench -p tracedecay-graph-db --bench publication_scale \
//!     --features test-helpers,hotpath

use std::time::Instant;

use tracedecay_graph_db::GraphGenerationManifest;

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

mod support;
#[path = "support/scale.rs"]
mod support_scale;

use support::PersistentBenchmarkGraph;
use support_scale::shard_manifest;

const SHARDS: usize = 24;
const ENTITIES_PER_SHARD: usize = 2_000;
const RELATIONS_PER_SHARD: usize = 2_000;

fn env_scale(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn main() {
    #[cfg(feature = "hotpath")]
    let _hotpath = hotpath::HotpathGuardBuilder::new("publication-scale").build();

    let shards = env_scale("PUBLICATION_SCALE_SHARDS", SHARDS);
    let entities_per_shard = env_scale("PUBLICATION_SCALE_ENTITIES", ENTITIES_PER_SHARD);
    let relations_per_shard = env_scale("PUBLICATION_SCALE_RELATIONS", RELATIONS_PER_SHARD);
    let mut graph = PersistentBenchmarkGraph::new();
    let mut first_publish_ms = 0.0_f64;
    let mut last_publish_ms = 0.0_f64;
    println!("shard,entities_total,publish_ms");
    for shard in 0..shards {
        let manifest: GraphGenerationManifest =
            shard_manifest(shard, 1, entities_per_shard, relations_per_shard);
        let started = Instant::now();
        drop(graph.publish_new_projection(manifest));
        let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
        if shard == 0 {
            first_publish_ms = elapsed;
        }
        last_publish_ms = elapsed;
        println!("{shard},{},{elapsed:.1}", (shard + 1) * entities_per_shard);
    }
    // Incremental update: replace one shard's generation while the full
    // corpus is resident, mirroring a steady-state watch-mode publication.
    let manifest = shard_manifest(0, 2, entities_per_shard, relations_per_shard);
    let started = Instant::now();
    drop(graph.publish_new_projection(manifest));
    let incremental_ms = started.elapsed().as_secs_f64() * 1_000.0;
    println!("incremental_republish_ms,{incremental_ms:.1}");
    println!(
        "scaling_ratio_last_over_first,{:.2}",
        last_publish_ms / first_publish_ms.max(0.001)
    );
    // Clean close: settle the store with a close/recover cycle so nothing is
    // staged, then time a close that changes nothing. This is the cost every
    // read-mostly consumer (daemon shutdown, workspace close, registry
    // eviction) pays at full corpus size; without Grafeo's container-current
    // skip it rewrites the entire accumulated store.
    drop(graph.recover_snapshot());
    let started = Instant::now();
    graph.close_store();
    let clean_close_ms = started.elapsed().as_secs_f64() * 1_000.0;
    println!("clean_close_ms,{clean_close_ms:.1}");
    // The skipped checkpoint must leave the store fully recoverable. A lane
    // whose recovery applies writes (sealed-proof adoption) pays one more
    // dirty checkpoint above; this second cycle isolates the truly clean
    // close every steady-state consumer sees.
    drop(graph.recover_snapshot());
    let started = Instant::now();
    graph.close_store();
    let second_clean_close_ms = started.elapsed().as_secs_f64() * 1_000.0;
    println!("second_clean_close_ms,{second_clean_close_ms:.1}");
    drop(graph.recover_snapshot());
}
