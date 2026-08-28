//! Sealed-generation verification is proven once per set of container bytes,
//! not once per open -- without ever weakening what a failed proof means.
//!
//! The full proof streams every stored row of a generation, canonicalizes it,
//! and SHA-256s the stream. It is the only evidence that the rows about to be
//! served are the rows the relational authority journaled. It is also work
//! proportional to the whole generation, and a sealed generation never
//! changes, so every restart re-derived a digest it had already derived.
//!
//! These tests pin the four properties that make skipping it sound:
//!
//! 1. a restart over untouched bytes hits the marker and enumerates nothing;
//! 2. a byte flipped on disk changes the container's identity, so the marker
//!    is refused and the full proof runs and fails closed;
//! 3. a marker forged to name a different digest is never believed, because
//!    the expected digest comes from the authority and never from the marker;
//! 4. publishing writes the marker, so the *next* open is the fast one.

use std::fs;
use std::io::{Seek, SeekFrom, Write};

use tracedecay_graph_db::take_graph_db_verification_counters;

use super::*;

/// Publishes one generation into a fresh store, closes it, and returns the
/// pieces a restart needs.
struct Published {
    temp: TempDir,
    registered: RegisteredGraph,
    authority: RelationalAuthority,
    key: GraphPublicationKeyV1,
}

fn publish_one(namespace: &str) -> Published {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection(namespace, "work");
    let generation = manifest(identity, "g1", "g1", vec![], vec![]);
    let record = stage_manifest(
        &mut authority,
        &registered.binding,
        &generation,
        "publish:g1",
        None,
        'd',
    );
    registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            None,
        )
        .unwrap();
    assert!(registered.close().unwrap());
    let key = record.publication.key.clone();
    Published {
        temp,
        registered,
        authority,
        key,
    }
}

/// Remounts and recovers, returning the outcome so a caller can assert on a
/// failure as readily as on success.
fn remount_and_recover(published: &mut Published) -> Result<(), GraphDbError> {
    published.registered.mount()?;
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    published
        .registered
        .registry
        .recover_verified_snapshot(
            registration(
                published.registered.binding.clone(),
                published.temp.path(),
            ),
            &mut published.authority,
            &context,
            &published.key.projection,
        )
        .map(|_| ())
}

fn marker_path(root: &std::path::Path) -> std::path::PathBuf {
    support::graph_path(root).with_extension("verified")
}

/// A publication proves the generation in full and files that proof, so the
/// marker is on disk once the store closes. Nothing is skipped here -- this is
/// the write that makes the *next* open cheap.
#[test]
fn publishing_and_closing_writes_the_verified_marker() {
    let published = publish_one("marker:written");
    let marker = marker_path(published.temp.path());

    assert!(
        marker.is_file(),
        "a closed store must leave the proof it established at {}",
        marker.display()
    );
    let body = fs::read(&marker).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // The marker binds itself to its own contents, and records the identity of
    // the container it was proven against.
    assert!(parsed["body_digest"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(parsed["body"]["generations"].as_array().unwrap().len(), 1);
    assert!(parsed["body"]["container"]["len"].as_u64().unwrap() > 0);
}

/// The point of the whole exercise: a restart over bytes that did not change
/// re-checks a stat instead of re-hashing a generation.
#[test]
fn a_restart_over_unchanged_bytes_hits_the_marker_and_enumerates_nothing() {
    let mut published = publish_one("marker:hit");

    // Discard the counters the publication itself produced; only the restart
    // is under test.
    let _ = take_graph_db_verification_counters();
    remount_and_recover(&mut published).unwrap();

    let counters = take_graph_db_verification_counters();
    assert_eq!(
        counters.full_verifications, 0,
        "an unchanged container must not re-stream a single generation's rows"
    );
    assert!(
        counters.marker_hits >= 1,
        "the restart must resolve through the marker, saw {counters:?}"
    );
    assert!(
        counters.marker_hit_bytes > 0,
        "a marker hit must report the canonical bytes it did not re-hash"
    );
}

/// The fail-closed case: a marker left beside corrupted bytes must not let
/// them through.
///
/// Worth recording what actually catches this, because it is not the marker
/// and it is not TraceDecay's SHA-256 proof. Grafeo CRC-32s every section of a
/// `.grafeo` container when it reads it, so a byte flipped anywhere in the
/// container fails the *open*, well before any generation is verified. That
/// layer was always there; the replay proof only ever ran after the CRC had
/// already passed, which is exactly why standing the marker in front of the
/// replay proof does not give up the defence against bit rot.
///
/// What this test therefore pins is the property that matters: corrupted bytes
/// are refused, and the stale marker sitting next to them is never believed.
#[test]
fn a_byte_flip_under_a_stale_marker_is_still_caught() {
    let mut published = publish_one("marker:flipped");
    let container = support::graph_path(published.temp.path());
    assert!(marker_path(published.temp.path()).is_file());

    // Corrupt the container in place, leaving the marker exactly as the clean
    // close wrote it. The marker is now stale, and must be treated as such.
    let original = fs::read(&container).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(&container).unwrap();
    let offset = (original.len() / 2) as u64;
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&[original[offset as usize] ^ 0xFF]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert_ne!(fs::read(&container).unwrap(), original);

    let _ = take_graph_db_verification_counters();
    let outcome = remount_and_recover(&mut published);

    assert!(
        outcome.is_err(),
        "a corrupted container must not be served, marker or no marker"
    );
    let counters = take_graph_db_verification_counters();
    assert_eq!(
        counters.marker_hits, 0,
        "a marker written against different bytes must never be believed"
    );
}

/// The identity gate on its own, with corruption held out of it.
///
/// Republishing the container byte-for-byte through a fresh inode leaves rows
/// that verify perfectly and a marker whose recorded identity no longer
/// matches the file. That must be a miss: the marker is refused, the full
/// proof runs over the rows, and recovery succeeds. This is the "identity
/// changes -> full hash" half of the contract, isolated from the question of
/// which layer notices bad bytes.
#[test]
fn a_container_with_a_new_identity_is_re_proven_even_though_its_bytes_verify() {
    let mut published = publish_one("marker:reinoded");
    let container = support::graph_path(published.temp.path());
    let staged = container.with_extension("grafeo-copy");

    // Same bytes, new inode -- and a new modification time.
    fs::copy(&container, &staged).unwrap();
    fs::rename(&staged, &container).unwrap();

    let _ = take_graph_db_verification_counters();
    remount_and_recover(&mut published).unwrap();

    let counters = take_graph_db_verification_counters();
    assert_eq!(
        counters.marker_hits, 0,
        "a marker must not vouch for a container it was not written against"
    );
    assert!(
        counters.full_verifications >= 1,
        "an unrecognised container must be proven from its rows, saw {counters:?}"
    );
    assert!(counters.full_verification_bytes > 0);
}

/// A marker naming some other digest cannot make that digest acceptable.
///
/// The expected digest comes from the relational authority on every path, and
/// the marker is only asked whether *that* digest was already proven. A forged
/// record therefore misses, the full proof runs, and -- because the rows on
/// disk are genuinely intact -- recovery still succeeds. The forgery bought
/// nothing except the work it was trying to skip.
#[test]
fn a_marker_forged_for_a_different_digest_is_refused_and_the_proof_runs() {
    let mut published = publish_one("marker:forged");
    let marker = marker_path(published.temp.path());

    let mut parsed: serde_json::Value =
        serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    parsed["body"]["generations"][0]["recovered_digest"] =
        serde_json::Value::String(format!("sha256:{}", "f".repeat(64)));
    // Re-seal the forgery so the self-digest is *not* what rejects it: this
    // test is about the authority comparison, not about torn writes.
    let body = parsed["body"].clone();
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    sha2::Digest::update(
        &mut hasher,
        b"tracedecay.graph-db.verified-generation-marker.v1\0",
    );
    sha2::Digest::update(&mut hasher, serde_json::to_vec(&body).unwrap());
    parsed["body_digest"] = serde_json::Value::String(format!(
        "sha256:{}",
        hex::encode(sha2::Digest::finalize(hasher))
    ));
    fs::write(&marker, serde_json::to_vec(&parsed).unwrap()).unwrap();

    let _ = take_graph_db_verification_counters();
    // The rows themselves were never touched, so the full proof succeeds.
    remount_and_recover(&mut published).unwrap();

    let counters = take_graph_db_verification_counters();
    assert_eq!(
        counters.marker_hits, 0,
        "a digest the authority did not ask for must never satisfy a lookup"
    );
    assert!(
        counters.full_verifications >= 1,
        "refusing the forgery must fall through to the full proof, saw {counters:?}"
    );
}

/// An absent marker is a cache miss, not a fault: recovery still works, it
/// just pays the proof again.
#[test]
fn a_deleted_marker_costs_a_full_proof_and_nothing_else() {
    let mut published = publish_one("marker:absent");
    fs::remove_file(marker_path(published.temp.path())).unwrap();

    let _ = take_graph_db_verification_counters();
    remount_and_recover(&mut published).unwrap();

    let counters = take_graph_db_verification_counters();
    assert_eq!(counters.marker_hits, 0);
    assert!(counters.full_verifications >= 1);
}
