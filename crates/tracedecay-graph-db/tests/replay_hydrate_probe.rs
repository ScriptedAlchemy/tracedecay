//! Phase breakdown of `GraphGenerationManifest::from_replay`, the
//! `graph_db.generation.replay.hydrate` hot path.
//!
//! Ignored by default; a measurement harness, not a contract. The at-rest
//! harnesses (`at_rest_snapshot.rs`, `at_rest_grafeo_probe.rs`) price the
//! *store's* close/reopen cycle and never reach the manifest replay path, so
//! hydrate had no reproducible measurement of its own.
//!
//! One scale per process, same reasoning as the at-rest harnesses: `VmHWM` is
//! process-wide and only ever rises, so a second scale in the same process
//! cannot report its own peak.
//!
//! ```text
//! TRACEDECAY_REPLAY_ROWS=500000 \
//!   cargo test -p tracedecay-graph-db --features test-helpers \
//!   --test replay_hydrate_probe -- --ignored --nocapture --exact replay_hydrate_probe
//! ```
//!
//! # Why this measures phases and not one inline round trip
//!
//! `MAX_GRAPH_REPLAY_SOURCE_BYTES_V1` is 4 MiB, so a corpus-scale generation
//! cannot travel as an inline replay payload at all — `relational_replay`
//! refuses it. Production hydrates a corpus through the *sealed* provider
//! instead: `from_replay` decodes a small pointer, hands off to
//! `hydrate_sealed_code_generation`, and then runs two corpus-scale sweeps of
//! its own over whatever that returns. Those two sweeps are what this probe
//! prices, because they are what is left of `replay.hydrate` once the sealed
//! decode underneath it is accounted for separately:
//!
//! * `manifest new` — `checked_sorted_entities` / `checked_sorted_relations`
//!   plus the `validate_checked` per-row sweep. The sealed provider pays this
//!   building the manifest it returns.
//! * `recovered digest` — `recovered_generation_digest`: a canonical re-encode
//!   of every entity and relation frame, absorbed into one SHA-256. Measured
//!   on a cold instance so no memo can serve it.
//!
//! A small inline round trip still runs at the end, as a identity check that
//! the phases were measured on the same shape production hydrates.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationDependency, GraphGenerationId,
    GraphGenerationManifest, GraphGenerationRelation, GraphIdempotencyKey, GraphLabel,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProperty, GraphPropertyName,
    GraphRelationId, GraphRelationKind, GraphWatermark, SourceGeneration,
};
use tracedecay_store::{GraphPublicationInputDigestV1, StoreShardIdV1};

/// Default entity count when `TRACEDECAY_REPLAY_ROWS` is unset.
const DEFAULT_ROWS: usize = 500_000;

/// Relations per entity, as a divisor, matching the at-rest harnesses.
const RELATION_DIVISOR: usize = 4;

/// Entity count for the inline round-trip identity check. Small on purpose:
/// the inline replay payload bound is 4 MiB.
const INLINE_ROWS: usize = 1_000;

fn status_kib(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let prefix = format!("{field}:");
    status
        .lines()
        .find(|line| line.starts_with(&prefix))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
}

fn peak_rss_kib() -> Option<u64> {
    status_kib("VmHWM")
}

fn live_rss_kib() -> Option<u64> {
    status_kib("VmRSS")
}

fn mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

fn identity() -> GraphProjectionIdentity {
    GraphProjectionIdentity::new(
        GraphNamespace::new("project").unwrap(),
        GraphProjectionId::new("code").unwrap(),
    )
}

fn entity_id(index: usize) -> GraphEntityId {
    GraphEntityId::new(format!("entity-{index:08}")).unwrap()
}

fn entity(index: usize) -> GraphEntity {
    let mut labels = BTreeSet::new();
    labels.insert(GraphLabel::new("Symbol").unwrap());
    let mut properties = BTreeMap::new();
    properties.insert(
        GraphPropertyName::new("name").unwrap(),
        GraphProperty::String(format!("symbol-{index}")),
    );
    properties.insert(
        GraphPropertyName::new("path").unwrap(),
        GraphProperty::String(format!("crates/probe/src/module-{}.rs", index % 4_096)),
    );
    GraphEntity::new(entity_id(index), labels, properties).unwrap()
}

fn relation(identity: &GraphProjectionIdentity, index: usize) -> GraphGenerationRelation {
    let mut properties = BTreeMap::new();
    properties.insert(
        GraphPropertyName::new("line").unwrap(),
        GraphProperty::String(format!("{}", index % 512)),
    );
    GraphGenerationRelation::new(
        GraphRelationId::new(format!("relation-{index:08}")).unwrap(),
        GraphEntityRef::new(identity.clone(), entity_id(index)),
        GraphEntityRef::new(identity.clone(), entity_id(index + 1)),
        GraphRelationKind::new("calls").unwrap(),
        properties,
    )
    .unwrap()
}

fn shard() -> StoreShardIdV1 {
    StoreShardIdV1::project(
        tracedecay_store::BrainId::new("brain.replay-probe").unwrap(),
        tracedecay_store::UserProfileId::new("profile.replay-probe").unwrap(),
        tracedecay_store::ProjectId::new("project.replay-probe").unwrap(),
    )
}

fn rows(
    identity: &GraphProjectionIdentity,
    count: usize,
) -> (Vec<GraphEntity>, Vec<GraphGenerationRelation>) {
    let entities = (0..count).map(entity).collect::<Vec<_>>();
    let relations = (0..count / RELATION_DIVISOR)
        .map(|index| relation(identity, index))
        .collect::<Vec<_>>();
    (entities, relations)
}

fn manifest(
    identity: &GraphProjectionIdentity,
    entities: Vec<GraphEntity>,
    relations: Vec<GraphGenerationRelation>,
) -> GraphGenerationManifest {
    GraphGenerationManifest::new(
        identity.clone(),
        GraphGenerationId::new("replay-probe-g1").unwrap(),
        SourceGeneration::new("replay-probe-source").unwrap(),
        GraphWatermark::new("replay-probe-watermark").unwrap(),
        vec![GraphGenerationDependency::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("project").unwrap(),
                GraphProjectionId::new("dependency").unwrap(),
            ),
            GraphGenerationId::new("dependency-g1").unwrap(),
            GraphIdempotencyKey::new("publish:dependency-g1").unwrap(),
        )],
        entities,
        relations,
    )
    .unwrap()
}

fn secs(duration: Duration) -> f64 {
    duration.as_secs_f64()
}

#[test]
#[ignore = "replay-hydrate measurement harness; run one scale per process, see module docs"]
fn replay_hydrate_probe() {
    let (Some(hwm_start), Some(rss_start)) = (peak_rss_kib(), live_rss_kib()) else {
        println!("/proc/self/status unavailable on this platform; skipping");
        return;
    };

    let count = std::env::var("TRACEDECAY_REPLAY_ROWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_ROWS);

    let identity = identity();

    let started = Instant::now();
    let (entities, relations) = rows(&identity, count);
    let build_wall = started.elapsed();
    let entity_count = entities.len();
    let relation_count = relations.len();

    // Sort plus the per-row validation sweep: what the sealed provider pays to
    // hand `from_replay` a manifest.
    let validate_started = Instant::now();
    let produced = manifest(&identity, entities, relations);
    let validate_wall = validate_started.elapsed();

    // The canonical re-encode of every row plus its SHA-256 absorption, on a
    // cold instance so the digest memo cannot serve it.
    let digest_started = Instant::now();
    let recovered = produced.expected_recovered_digest(&|| Ok(())).unwrap();
    let digest_wall = digest_started.elapsed();

    // Re-reading a proved digest must be free; a regression here would make
    // every figure above unrepresentative of one hydrate.
    let memo_started = Instant::now();
    let memoized = produced.expected_recovered_digest(&|| Ok(())).unwrap();
    let memo_wall = memo_started.elapsed();

    let hwm = peak_rss_kib().unwrap();
    let rss = live_rss_kib().unwrap();
    let (produced_entities, produced_relations) = produced.row_counts();
    drop(produced);

    // Identity: the same shape, small enough to travel inline, hydrates back
    // to an equal manifest and the same recovered digest.
    let (inline_entities, inline_relations) = rows(&identity, INLINE_ROWS);
    let inline = manifest(&identity, inline_entities, inline_relations);
    let inline_digest = inline.expected_recovered_digest(&|| Ok(())).unwrap();
    let replay = inline
        .relational_replay(
            shard(),
            GraphIdempotencyKey::new("publish:replay-probe-g1").unwrap(),
            GraphPublicationInputDigestV1::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            None,
            &|| Ok(()),
        )
        .unwrap();
    let payload_bytes = replay.canonical_replay_source.len();
    let hydrate_started = Instant::now();
    let hydrated = GraphGenerationManifest::from_inline_replay(&replay, &|| Ok(())).unwrap();
    let hydrate_wall = hydrate_started.elapsed();

    println!("=== graph generation replay-hydrate probe ===");
    println!("rows              : {entity_count} entities + {relation_count} relations");
    println!("fixture build     : {:.2}s", secs(build_wall));
    println!(
        "manifest new      : {:.2}s  <- sort + per-row validation sweep",
        secs(validate_wall)
    );
    println!(
        "recovered digest  : {:.2}s  <- cold canonical re-encode + sha256",
        secs(digest_wall)
    );
    println!(
        "digest re-read    : {:.4}s  <- memoized, must be ~0",
        secs(memo_wall)
    );
    println!(
        "VmHWM delta       : {} KiB ({:.1} MiB)",
        hwm - hwm_start,
        mib(hwm - hwm_start)
    );
    println!("VmRSS             : {rss} KiB ({:.1} MiB)", mib(rss));
    println!(
        "VmRSS baseline    : {rss_start} KiB ({:.1} MiB)",
        mib(rss_start)
    );
    println!("--- inline identity check ({INLINE_ROWS} entities) ---");
    println!(
        "inline payload    : {payload_bytes} bytes ({:.2} MiB)",
        payload_bytes as f64 / (1024.0 * 1024.0)
    );
    println!("inline hydrate    : {:.4}s", secs(hydrate_wall));

    assert_eq!(produced_entities, entity_count, "fixture lost entities");
    assert_eq!(produced_relations, relation_count, "fixture lost relations");
    assert_eq!(
        recovered, memoized,
        "a memoized digest re-read must equal the computed one"
    );
    assert_eq!(
        hydrated, inline,
        "inline replay did not hydrate an equal manifest"
    );
    assert_eq!(
        hydrated.expected_recovered_digest(&|| Ok(())).unwrap(),
        inline_digest,
        "hydration must pin the recovered digest its source proved"
    );
}
