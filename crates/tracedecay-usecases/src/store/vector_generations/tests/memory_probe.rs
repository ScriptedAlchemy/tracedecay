/// Peak resident set size of this process, in bytes.
fn peak_resident_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmHWM:"))
                .and_then(|value| value.split_whitespace().next())
                .and_then(|kilobytes| kilobytes.parse::<u64>().ok())
        })
        .map(|kilobytes| kilobytes * 1024)
        .unwrap_or_default()
}

/// Build one prepared batch covering `range` of the probe corpus.
fn probe_prepared_batch(
    embedding: &AdmittedEmbeddingProjectionKeyV1,
    projection_key: &ProjectionKeyV1,
    source: &CodeGenerationId,
    dimensions: u32,
    range: std::ops::Range<usize>,
) -> PreparedVectorGenerationV1 {
    let mut vectors = Vec::with_capacity(range.len());
    let mut decisions = Vec::with_capacity(range.len());
    let mut changed = Vec::with_capacity(range.len());
    for index in range {
        let chunk_id: CodeSearchChunkId = id(&format!("chunk.v1.probe-{index:06}"));
        let chunk_digest: ContentDigest = id(&format!("sha256:{index:064x}"));
        let values = (0..dimensions)
            .map(|dimension| (index as f32 + dimension as f32) * 1.0e-4)
            .collect::<Vec<_>>();
        let output_digest = tracedecay_semantic::projector::vector_output_digest(
            projection_key,
            &chunk_id,
            &chunk_digest,
            &values,
        )
        .expect("output digest");
        changed.push(ChangedCodeChunkV1 {
            chunk_id: chunk_id.clone(),
            prior_digest: None,
            current_digest: Some(chunk_digest.clone()),
        });
        decisions.push(
            tracedecay_code_index::projection::ChunkProjectionDecisionV1 {
                chunk_id: chunk_id.clone(),
                prior_chunk_digest: None,
                current_chunk_digest: Some(chunk_digest.clone()),
                operation: ProjectionOperationV1::Added,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(output_digest.clone()),
            },
        );
        vectors.push(ProjectedChunkVectorV1 {
            projection_key: projection_key.clone(),
            source_generation: source.clone(),
            source_manifest_digest: manifest_digest('0'),
            chunk_id,
            chunk_digest,
            values,
            output_digest,
        });
    }
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: None,
        to_generation: source.clone(),
        manifest_digest: manifest_digest('0'),
        added_or_changed: changed,
        deleted: vec![],
        reused: vec![],
    };
    changes.manifest_digest = changes.compute_digest().expect("changed-set digest");
    for vector in &mut vectors {
        vector.source_manifest_digest = changes.manifest_digest.clone();
    }
    let mut request = ProjectionBatchRequestV1 {
        request_digest: manifest_digest('0'),
        changes,
        previous_projection_key: None,
        target_projection_key: projection_key.clone(),
        replay_reason: ProjectionReplayReasonV1::SourceEdit,
    };
    request.request_digest = tracedecay_code_index::projection::expected_request_digest(&request)
        .expect("request digest");
    let receipt = tracedecay_code_index::projection::build_batch_receipt(&request, &decisions)
        .expect("batch receipt");
    PreparedVectorGenerationV1 {
        embedding_key: embedding.clone(),
        request,
        receipt,
        vectors,
        tombstones: vec![],
    }
}

/// Scale probe for a whole-corpus vector generation committed in batches.
///
/// Reports peak RSS and, per commit, the size of the state document the
/// mutation binds. The document size is the number that used to grow with
/// the corpus until it hit `MAX_REQUEST_BYTES`; with the metadata
/// externalized it should stay flat no matter how many batches land.
///
/// Ignored by default: it is a measurement, not an assertion about the
/// host. Run it with `--ignored --nocapture`, optionally with
/// `VECTOR_RSS_PROBE_CHUNKS` and `VECTOR_RSS_PROBE_BATCH`.
#[tokio::test]
#[ignore = "memory probe; run explicitly"]
async fn probe_peak_resident_bytes_for_a_whole_corpus_generation() {
    let chunks: usize = std::env::var("VECTOR_RSS_PROBE_CHUNKS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5_000);
    let batch: usize = std::env::var("VECTOR_RSS_PROBE_BATCH")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(chunks)
        .max(1);
    #[expect(non_snake_case, reason = "probe keeps the constant-style names")]
    let CHUNKS = chunks;
    const DIMENSIONS: u32 = 768;
    let temporary = tempfile::tempdir().expect("temporary project database");
    let (database, _authority) = open_project_database(&temporary, "vector rss probe").await;
    let mut key = admitted_embedding().embedding_key().clone();
    key.dimensions = DIMENSIONS;
    let embedding = key.admit().expect("admitted probe embedding");
    let projection_key = embedding.projection_key().clone();
    let source: CodeGenerationId = id("code-generation.rss-probe");

    let mut chunk_ids = (0..CHUNKS)
        .map(|index| id::<CodeSearchChunkId>(&format!("chunk.v1.probe-{index:06}")))
        .collect::<Vec<_>>();
    chunk_ids.sort();
    // The plan's watermark is the corpus's, not any one batch's, so
    // splitting the run never moves the generation identity.
    let whole = probe_prepared_batch(&embedding, &projection_key, &source, DIMENSIONS, 0..CHUNKS);
    let source_manifest_digest = whole.request.changes.manifest_digest.clone();
    drop(whole);
    let plan = VectorGenerationPlanV1 {
        target_projection_key: projection_key.clone(),
        source_generation: source.clone(),
        source_manifest_digest,
        expected_chunk_ids: chunk_ids.into(),
        base_generation: None,
    };

    let baseline = peak_resident_bytes();
    let store = DatabaseVectorGenerationStoreV1::open(&database)
        .await
        .expect("open store");
    let build = store.begin_generation(plan).await.expect("build identity");
    let mut checkpoint = None;
    let mut widest_document = 0_usize;
    let mut commits = 0_usize;
    let mut start = 0;
    while start < CHUNKS {
        let end = (start + batch).min(CHUNKS);
        let prepared =
            probe_prepared_batch(&embedding, &projection_key, &source, DIMENSIONS, start..end);
        checkpoint = Some(
            store
                .commit_batch(&build, checkpoint.as_ref(), prepared)
                .await
                .expect("commit batch"),
        );
        widest_document = widest_document.max(state_document(&database).await.len());
        commits += 1;
        start = end;
    }
    let publication = store
        .publish_generation(&build, None)
        .await
        .expect("publish corpus");
    widest_document = widest_document.max(state_document(&database).await.len());
    let peak = peak_resident_bytes();
    println!(
        "vector-generation scale probe: chunks={CHUNKS} batch={batch} commits={commits} \
             dimensions={DIMENSIONS} float_payload_bytes={} widest_state_document_bytes={} \
             peak_rss_bytes={peak} peak_rss_gib={:.2} baseline_rss_bytes={baseline} \
             generation={}",
        CHUNKS * DIMENSIONS as usize * size_of::<f32>(),
        widest_document,
        peak as f64 / (1024.0 * 1024.0 * 1024.0),
        publication.generation_id.as_digest(),
    );
}

async fn state_revision(database: &Database) -> i64 {
    let mut rows = database
        .engine_conn()
        .query(
            "SELECT revision FROM semantic_vector_generation_state_v1 WHERE singleton = 1",
            (),
        )
        .await
        .expect("revision");
    let row = rows.next().await.expect("revision row").expect("row");
    row.get::<i64>(0).expect("revision")
}
