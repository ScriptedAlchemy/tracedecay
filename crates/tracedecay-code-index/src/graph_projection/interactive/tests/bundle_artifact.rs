//! Seal-time catalog artifact: the manifest-derived bundle artifact must
//! install as a ready catalog identical to the one the projection warm scan
//! builds, and a foreign or corrupt artifact must be a typed refusal.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_graph_db::NeverCancelled;

use super::*;
use crate::graph_projection::interactive::artifact::decode_interactive_catalog_artifact;
use crate::graph_projection::{
    INTERACTIVE_CATALOG_ARTIFACT_NAME, code_graph_generation_id, write_interactive_catalog_artifact,
};

fn encoded_fixture_artifact() -> Vec<u8> {
    let manifest = production_manifest();
    let mut bytes = Vec::new();
    write_interactive_catalog_artifact(&manifest, &mut bytes, &NeverCancelled)
        .expect("encode catalog artifact");
    bytes
}

struct CancelAfterArtifactOutputStarts<'a>(&'a AtomicBool);

impl GraphCancellation for CancelAfterArtifactOutputStarts<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct CancellingWriter<'a> {
    output_started: &'a AtomicBool,
    bytes_written: usize,
}

impl Write for CancellingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes_written += bytes.len();
        self.output_started.store(true, Ordering::Release);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn artifact_rows_stream_and_observe_cancellation_after_output_starts() {
    let manifest = production_manifest();
    let output_started = AtomicBool::new(false);
    let cancellation = CancelAfterArtifactOutputStarts(&output_started);
    let mut writer = CancellingWriter {
        output_started: &output_started,
        bytes_written: 0,
    };

    let error = write_interactive_catalog_artifact(&manifest, &mut writer, &cancellation)
        .expect_err("streamed artifact emission must stop after cancellation");

    assert_eq!(error, CodeGraphProjectionError::Cancelled);
    assert!(writer.bytes_written > 0, "artifact output never started");
}

/// Measurement harness, not a regression gate. Prices the catalog-at-seal
/// trade at generation scale over the same snapshot/catalog code the daemon
/// runs: the open-time paged warm scan against the seal-time linear
/// derivation+encode and the open-time verified install.
///
/// ```text
/// TRACEDECAY_CATALOG_BENCH_SYMBOLS=200000 cargo test -p tracedecay-code-index \
///   --release --lib bundle_artifact::measure_catalog_seal_vs_open_at_scale \
///   -- --ignored --nocapture
/// ```
#[test]
#[ignore = "measurement harness: run with --ignored --nocapture"]
fn measure_catalog_seal_vs_open_at_scale() {
    use std::time::Instant;

    let symbols: usize = std::env::var("TRACEDECAY_CATALOG_BENCH_SYMBOLS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100_000);
    let files = symbols.div_ceil(20);
    let projection =
        code_graph_projection_identity(GraphNamespace::new("code-graph").expect("namespace"))
            .expect("projection identity");
    let bench_files: Vec<SanitizedCodeFileV1> = (0..files)
        .map(|index| file(&format!("file.{index}"), &format!("src/module_{index}.rs")))
        .collect();
    let bench_symbols = GenerationSymbolIndexV1::new(
        generation(),
        (0..symbols)
            .map(|index| {
                let mut record = symbol_metadata(
                    &format!("sym.{index}"),
                    &format!("bench::module_{}::symbol_{index}", index / 20),
                    "function",
                    'a',
                );
                record.identity = id(&format!("sha256:{index:064x}"));
                Arc::new(record)
            })
            .collect(),
    )
    .expect("bench symbol index");
    let bench_chunks: Vec<Arc<CodeSearchChunkV1>> = (0..symbols)
        .map(|index| {
            Arc::new(chunk(
                &format!("sym.{index}"),
                &format!("file.{}", index / 20),
                (index % 20) as u32,
            ))
        })
        .collect();
    let bench_edges: Vec<CanonicalRelationEdgeV1> = (1..symbols)
        .map(|index| {
            edge(
                &format!("sym.{}", index - 1),
                &format!("sym.{index}"),
                RelationEdgeKindV1::Calls,
                (index * 2) as u64,
            )
        })
        .collect();
    let build = Instant::now();
    let manifest = build_code_graph_manifest_inputs_checked(
        projection,
        &generation(),
        &bench_edges,
        &bench_chunks,
        Some(ProductionCodeGraphInputs {
            files: &bench_files,
            symbols: &bench_symbols,
            imports: &[],
        }),
        &GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
            .expect("projector revision"),
        &|| Ok(()),
    )
    .expect("bench manifest");
    let manifest_build = build.elapsed();

    // Seal-time cost added: derive the catalog from manifest rows and encode.
    let seal = Instant::now();
    let mut bytes = Vec::new();
    write_interactive_catalog_artifact(&manifest, &mut bytes, &NeverCancelled)
        .expect("encode catalog artifact");
    let seal_cost = seal.elapsed();
    let artifact_bytes = bytes.len();

    // Open-time cost removed: the paged projection warm scan.
    let warm_store = store_for(manifest.clone());
    let warm = Instant::now();
    warm_store
        .warm_interactive_catalog_with_cancellation(request())
        .expect("warm catalog");
    let warm_cost = warm.elapsed();

    // Open-time cost remaining: decode + revalidate + install.
    let install_store = store_for(manifest);
    let install = Instant::now();
    install_store
        .install_interactive_catalog_artifact(&bytes, request())
        .expect("install catalog artifact");
    let install_cost = install.elapsed();
    assert_eq!(install_store.interactive_catalog_scan_builds(), 0);

    println!("--- catalog seal-vs-open @ {symbols} symbols / {files} files ---");
    println!("manifest build          : {manifest_build:?}");
    println!("seal add (derive+encode): {seal_cost:?}");
    println!("artifact bytes          : {artifact_bytes}");
    println!("open warm scan (before) : {warm_cost:?}");
    println!("open install (after)    : {install_cost:?}");
    println!(
        "open speedup            : {:.1}x",
        warm_cost.as_secs_f64() / install_cost.as_secs_f64().max(f64::EPSILON)
    );
}

#[test]
fn artifact_name_is_the_bundle_contract() {
    assert_eq!(INTERACTIVE_CATALOG_ARTIFACT_NAME, "interactive-catalog");
}

#[test]
fn installed_artifact_serves_identically_to_the_warm_scan_without_scanning() {
    let bytes = encoded_fixture_artifact();

    let warmed = store_for(production_manifest());
    warmed
        .warm_interactive_catalog_with_cancellation(request())
        .expect("warm catalog");

    let installed = store_for(production_manifest());
    installed
        .install_interactive_catalog_artifact(&bytes, request())
        .expect("install catalog artifact");
    assert!(
        installed
            .interactive_catalog_is_warm()
            .expect("catalog state readable"),
        "an installed artifact must be a ready catalog"
    );
    assert_eq!(
        installed.interactive_catalog_scan_builds(),
        0,
        "installing a bundle artifact must not run the projection warm scan"
    );
    assert!(
        warmed.interactive_catalog_scan_builds() > 0,
        "the warm control store really did scan the projection"
    );

    let warmed_reader = reader(&warmed);
    let installed_reader = reader(&installed);
    for (label, from_warm, from_install) in [
        (
            "qualified name",
            warmed_reader
                .resolve_qualified_name("beta::run", None, 8, request())
                .expect("warm resolve"),
            installed_reader
                .resolve_qualified_name("beta::run", None, 8, request())
                .expect("installed resolve"),
        ),
        (
            "simple name",
            warmed_reader
                .resolve_simple_name("Runner", None, 8, request())
                .expect("warm resolve"),
            installed_reader
                .resolve_simple_name("Runner", None, 8, request())
                .expect("installed resolve"),
        ),
        (
            "logical file listing",
            warmed_reader
                .symbols_in_logical_file("src/beta.rs", 8, request())
                .expect("warm listing"),
            installed_reader
                .symbols_in_logical_file("src/beta.rs", 8, request())
                .expect("installed listing"),
        ),
    ] {
        assert_eq!(from_warm, from_install, "{label} diverged");
        assert!(!from_warm.is_empty(), "{label} fixture must resolve");
    }
    let warm_page = warmed_reader
        .symbols_page(None, 64, request())
        .expect("warm page");
    let installed_page = installed_reader
        .symbols_page(None, 64, request())
        .expect("installed page");
    assert_eq!(warm_page, installed_page);
    assert_eq!(
        warmed_reader.files(64, request()).expect("warm files"),
        installed_reader
            .files(64, request())
            .expect("installed files"),
    );

    // Idempotent over an already-ready catalog.
    installed
        .install_interactive_catalog_artifact(&bytes, request())
        .expect("reinstall is idempotent");
}

#[test]
fn artifact_for_a_foreign_generation_is_a_typed_mismatch() {
    let bytes = encoded_fixture_artifact();
    let foreign = code_graph_generation_id(
        &id::<CodeGenerationId>("generation.interactive.other"),
        &GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
            .expect("projector revision"),
    )
    .expect("foreign generation id");
    let error = match decode_interactive_catalog_artifact(&bytes, foreign.as_str(), &NeverCancelled)
    {
        Ok(_) => panic!("foreign generation must be refused"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        CodeGraphProjectionError::GenerationMismatch
    ));
}

#[test]
fn corrupt_artifact_bytes_are_a_typed_corruption() {
    let store = store_for(production_manifest());
    let error = store
        .install_interactive_catalog_artifact(b"{\"not\": \"a catalog\"}", request())
        .expect_err("corrupt artifact must be refused");
    assert!(matches!(error, CodeGraphProjectionError::Corrupt(_)));
    assert!(
        !store
            .interactive_catalog_is_warm()
            .expect("catalog state readable"),
        "a refused artifact must not publish a catalog"
    );
}
