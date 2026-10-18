use super::super::clone_census::tests::payload;
use super::super::clone_codec::encode_clone_payload;
use super::super::format::{decode_term_lists, sourceless_test_receipt, term_lists_bytes};
use super::super::{
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeLexicalArtifactReaderV1,
    CodeLexicalCloneIndexCensusV1,
};
use super::*;
use rusqlite::StatementStatus;
use rusqlite::hooks::{AuthAction, Authorization};
use tracedecay_domain::{
    CodeGenerationId, ComponentRevision, FreshnessCompatibilityV1, ScoreDomainId, SourceFreshness,
    SourceInstanceKey, SourceNamespace, UtcMicros,
};

struct ActiveControl;

#[test]
fn carried_artifact_is_incompatible_even_at_the_current_revision() {
    let connection = Connection::open_in_memory().expect("SQLite");
    create_schema(&connection).expect("current artifact schema");
    verify_artifact_table_layout(&connection).expect("ordinary artifact is compatible");
    connection
        .execute_batch("CREATE TABLE carried_document_shift(singleton INTEGER PRIMARY KEY, shift INTEGER NOT NULL)")
        .expect("carried marker");
    assert!(matches!(
        verify_artifact_table_layout(&connection),
        Err(CodeLexicalArtifactErrorV1::Incompatible(_))
    ));
}

#[test]
fn interrupted_parent_copy_leaves_no_initializing_file() {
    struct CancelOnSecondCheck(std::sync::atomic::AtomicUsize);
    impl CodeIndexExecutionControlV1 for CancelOnSecondCheck {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 1
        }
        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let parent_path = directory.path().join("parent");
    let parent_size = 4 * 1024 * 1024 + 1;
    std::fs::write(&parent_path, vec![7u8; parent_size]).expect("parent bytes");
    let staging = directory.path().join("successor.staging");

    let copied = copy_sealed_parent(
        File::open(&parent_path).expect("parent"),
        &ManifestDigest::from_sha256_bytes(&[0u8; 32]).expect("digest"),
        parent_size as u64,
        &staging,
        &CancelOnSecondCheck(std::sync::atomic::AtomicUsize::new(0)),
    );

    assert!(matches!(
        copied,
        Err(CodeLexicalArtifactErrorV1::Interrupted(
            tracedecay_code_index::production::CodeIndexInterruptionV1::Cancelled
        ))
    ));
    assert!(!staging.exists(), "an interrupted copy is never named");
    assert!(
        !initializing_staging_sibling(&staging)
            .expect("sibling")
            .exists(),
        "an interrupted copy must not leave its partial bytes behind"
    );
}

impl CodeIndexExecutionControlV1 for ActiveControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

/// A merge wake keeps its statement journals in memory only while its
/// staged runs, plus the per-run sorter allowance, fit the sorter budget;
/// the n-gram rebuild sorts in Rust, and every other wake keeps the
/// file-backed sorter.
#[test]
fn merge_wakes_use_memory_temp_storage_only_within_the_sorter_budget() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE term_posting_runs(page_ordinal INTEGER, term TEXT, field INTEGER, postings BLOB);
             INSERT INTO term_posting_runs VALUES (0, 'alpha', 4, X'0102'), (1, 'beta', 4, X'03');
             CREATE TABLE exact_posting_runs(page_ordinal INTEGER, term_id INTEGER, field INTEGER, documents BLOB);
             INSERT INTO exact_posting_runs VALUES (0, 9, 1, X'010203');",
        )
        .unwrap();
    let fits = |step, budget| merge_wake_fits_memory_temp_store(&connection, step, budget).unwrap();
    let indexes = |ordinal| Some((PersistedFinalizationPhaseV1::Indexes, ordinal));

    assert!(fits(indexes(1), 140));
    assert!(!fits(indexes(1), 139));
    assert!(fits(indexes(2), 67));
    assert!(!fits(indexes(2), 66));
    assert!(fits(indexes(3), 0));
    assert!(!fits(indexes(0), usize::MAX));
    assert!(!fits(
        Some((PersistedFinalizationPhaseV1::Digest, 1)),
        usize::MAX
    ));
    assert!(!fits(None, usize::MAX));
}

#[test]
fn clone_payload_conflict_checks_cover_full_and_partial_batches() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(
        "CREATE TABLE clone_body_payloads(payload_digest BLOB PRIMARY KEY, payload BLOB NOT NULL)",
    ).unwrap();
    let transaction = connection.transaction().unwrap();
    let bodies: Vec<_> = (0..=PAYLOAD_DIGEST_CONFLICT_CHECK_CHUNK)
        .map(|index| PreparedCloneBodyV1 {
            payload_digest: format!("sha256:{index:064x}"),
            payload: vec![1],
            eligibility: Vec::new(),
            serialized_bytes: 1,
            symbol_occurrence_id: format!("symbol.{index}"),
            path: "src/lib.rs".to_owned(),
            body_start: 0,
            body_end: 1,
            exact_keys: Vec::new(),
        })
        .collect();
    for body in &bodies {
        transaction
            .execute(
                "INSERT INTO clone_body_payloads VALUES (?1, ?2)",
                params![
                    stored_digest_key(&body.payload_digest).unwrap(),
                    body.payload
                ],
            )
            .unwrap();
    }
    let references: Vec<_> = bodies.iter().collect();
    verify_clone_payload_digests(&transaction, &references, &ActiveControl).unwrap();
    verify_clone_payload_digests(&transaction, &references[..1], &ActiveControl).unwrap();
    transaction
        .execute("UPDATE clone_body_payloads SET payload = X'02'", [])
        .unwrap();
    for batch in [&references[..], &references[..1]] {
        assert!(matches!(
            verify_clone_payload_digests(&transaction, batch, &ActiveControl),
            Err(CodeLexicalArtifactErrorV1::Corrupt(_))
        ));
    }
}

fn test_metadata() -> CodeLexicalProjectionMetadataV1 {
    CodeLexicalProjectionMetadataV1 {
        generation: CodeGenerationId::new("generation.artifact-builder.v1").expect("generation"),
        repository_id: None,
        logical_paths: Default::default(),
        freshness: SourceFreshness {
            source_namespace: SourceNamespace::new("namespace.artifact-builder")
                .expect("namespace"),
            source_instance: SourceInstanceKey::new("instance.artifact-builder").expect("instance"),
            source_watermark: None,
            projection_watermark: None,
            observed_at: UtcMicros(0),
            source_generation: None,
            generation_lag: None,
            compatibility: FreshnessCompatibilityV1::Unknown,
            policy_revision: ComponentRevision::new("policy.artifact-builder.v1").expect("policy"),
        },
        exact_retriever_revision: ComponentRevision::new("retriever.exact.artifact.v1")
            .expect("exact retriever"),
        lexical_retriever_revision: ComponentRevision::new("retriever.lexical.artifact.v1")
            .expect("lexical retriever"),
        exact_score_domain: ScoreDomainId::new("score.exact.artifact.v1").expect("score domain"),
        clone_route: None,
    }
}

fn create_mutable_test_schema(connection: &Connection) -> BuilderMutationGuardV1 {
    let gate = register_builder_mutation_gate(connection).expect("register builder mutation gate");
    create_schema(connection).expect("create artifact schema");
    BuilderMutationGuardV1::enter(&gate).expect("enter test builder mutation authority")
}

#[test]
fn field_statistics_preserve_posting_sums_and_omit_absent_fields() {
    for empty in [false, true] {
        let mut connection = Connection::open_in_memory().expect("statistics database");
        let _authority = create_mutable_test_schema(&connection);
        if !empty {
            // Two committed batches: the staged totals must accumulate
            // across them exactly as a scan over every posting would.
            let batches: [&[(i64, i64, i64, i64)]; 2] = [
                &[(1, 1, 1, 2), (2, 1, 1, 3), (1, 3, 1, 7)],
                &[(1, 1, 2, 4), (2, 7, 2, 11)],
            ];
            for batch in batches {
                let transaction = connection.transaction().expect("batch transaction");
                let mut totals = BTreeMap::new();
                for (_, field, _, frequency) in batch {
                    *totals.entry(*field).or_insert(0) += frequency;
                }
                stage_field_totals(&transaction, &totals).expect("stage field totals");
                transaction.commit().expect("commit batch");
            }
        }
        let expected: Vec<(i64, i64)> = if empty {
            Vec::new()
        } else {
            vec![(1, 2 + 3 + 4), (3, 7), (7, 11)]
        };
        let transaction = connection.transaction().expect("statistics transaction");
        derive_statistics_step(&transaction, 0, &ActiveControl).expect("derive statistics");
        let actual = transaction
            .prepare("SELECT field, total_length FROM field_stats ORDER BY field")
            .expect("derived sums")
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
            .expect("derived rows")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("derived totals");
        assert_eq!(actual, expected);
        assert!(
            transaction
                .execute("INSERT INTO field_stats VALUES (2, 5)", [])
                .is_err()
        );
        assert!(
            !table_exists(&transaction, "field_stats_staging").expect("staging lookup"),
            "sealing the statistics must drop the staging totals"
        );
    }
}

/// SQLite's Unix VFS cannot open a full name of 512 bytes or more and
/// says only "unable to open database file", which the scheduler kept
/// retrying as an unavailable store. The builder names the limit as a
/// contract failure, which ends the build.
#[cfg(unix)]
#[test]
fn create_refuses_a_staging_path_sqlite_cannot_address() {
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let mut parent = directory.path().to_path_buf();
    while parent.as_os_str().len() < 520 {
        parent.push("d".repeat(64));
    }
    std::fs::create_dir_all(&parent).expect("deep staging parent");
    let path = parent.join("staging.sqlite");

    let error = match CodeLexicalArtifactBuilderV1::create(&path, test_metadata()) {
        Ok(_) => panic!("SQLite cannot open a staging path this long"),
        Err(error) => error,
    };

    let CodeLexicalArtifactErrorV1::Contract(detail) = error else {
        panic!("expected a contract refusal naming the SQLite limit, got {error:?}");
    };
    assert!(
        detail.starts_with(&format!("SQLite database path '{}", path.display()))
            && detail.ends_with("SQLite's default VFS opens names shorter than 512 bytes"),
        "{detail}"
    );
}

#[test]
fn resume_refuses_staging_without_incremental_field_statistics() {
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let path = directory.path().join("no-field-stats-staging.sqlite");
    let metadata = test_metadata();
    drop(
        CodeLexicalArtifactBuilderV1::create(&path, metadata.clone())
            .expect("create current staging artifact"),
    );
    let connection = Connection::open(&path).expect("open staging artifact for fixture setup");
    connection
        .execute_batch("DROP TABLE field_stats_staging;")
        .expect("install pre-incremental staging layout");
    drop(connection);

    let error = match CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &path,
        metadata,
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &ActiveControl,
    ) {
        Ok(_) => panic!("resume must not seal statistics without staged totals"),
        Err(error) => error,
    };
    assert!(matches!(error, CodeLexicalArtifactErrorV1::Incompatible(_)));
}

#[test]
fn canonical_write_limit_refuses_ngram_receipt_past_the_exact_boundary() {
    let ngram_receipt_bytes = "sha256:".len() + 64;
    let mut ledger = CanonicalBatchLimitLedgerV1::default();
    assert!(
        ledger
            .try_admit(
                0,
                CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1 - ngram_receipt_bytes,
            )
            .expect("admit bytes below the ngram receipt boundary")
            .is_none()
    );
    let exceeded = ledger
        .try_admit(0, ngram_receipt_bytes + 1)
        .expect("evaluate ngram receipt boundary")
        .expect("one byte past the write boundary must be refused");
    assert_eq!(
        exceeded.limit,
        CodeLexicalArtifactBatchLimitV1::EstimatedWriteBytes
    );
    assert_eq!(
        exceeded.required,
        CODE_LEXICAL_ARTIFACT_MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES_V1 + 1
    );
}

#[test]
fn receipt_verification_does_not_read_append_only_base_tables() {
    let connection = Connection::open_in_memory().expect("open artifact database");
    let _mutation_authority = create_mutable_test_schema(&connection);
    connection
        .authorizer(Some(
            |context: rusqlite::hooks::AuthContext<'_>| match context.action {
                // The vocabulary is the sealed terms and their fuzzy
                // flags; their posting lists stay unread.
                AuthAction::Read {
                    table_name: "term_postings",
                    column_name,
                } if column_name != "lists" => Authorization::Allow,
                AuthAction::Read { table_name, .. }
                    if BASE_SECTION_NAMES.contains(&table_name)
                        || table_name.ends_with("_runs") =>
                {
                    Authorization::Deny
                }
                _ => Authorization::Allow,
            },
        ))
        .expect("deny exhaustive base-table verification reads");

    let sections = compute_section_digests(&connection, &ActiveControl)
        .expect("verify only source-page receipts and derived sections");
    assert_eq!(
        sections
            .iter()
            .map(|section| section.name.as_str())
            .collect::<Vec<_>>(),
        SECTION_NAMES
    );
}

/// The seal binds its clone census into the receipt, so an open serves
/// that census without walking a clone table. Here the one stored payload
/// is not the payload its digest names, which any recomputation refuses;
/// the open must still serve the sealed census, and an explicit
/// verification must still find the corruption.
#[test]
fn open_serves_the_sealed_clone_census_and_verification_recomputes_it() {
    let directory = tempfile::tempdir().expect("artifact directory");
    let path = directory.path().join("sealed.sqlite");
    drop(create_private_file_retained(&path).expect("private artifact file"));
    let metadata = test_metadata();
    let metadata_digest = metadata_digest(&metadata).expect("metadata digest");
    let connection = Connection::open(&path).expect("artifact database");
    {
        let _mutation_authority = create_mutable_test_schema(&connection);
        connection
            .execute(
                "INSERT INTO artifact_state(singleton, format_revision, metadata, metadata_digest, receipt) VALUES (1, ?1, ?2, ?3, ?4)",
                params![
                    i64::from(CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1),
                    content_metadata_bytes(&metadata).expect("metadata bytes"),
                    metadata_digest.as_str(),
                    vec![0u8; RECEIPT_RESERVATION_BYTES],
                ],
            )
            .expect("stage artifact state");
        let (stored, _) = encode_clone_payload(&payload(0)).expect("payload bytes");
        connection
            .execute(
                "INSERT INTO clone_body_payloads(payload_digest, payload) VALUES (?1, ?2)",
                params![
                    digest_key(&payload(1).payload_digest).expect("digest key"),
                    stored
                ],
            )
            .expect("stage a payload under another payload's digest");
    }
    let sealed_census = CodeLexicalCloneIndexCensusV1 {
        unique_payloads: 1,
        ..CodeLexicalCloneIndexCensusV1::default()
    };
    let receipt = sourceless_test_receipt(
        metadata_digest,
        compute_section_digests(&connection, &ActiveControl).expect("section digests"),
        sealed_census.clone(),
        sqlite_file_size(&connection).expect("file size"),
    )
    .expect("sealed receipt");
    connection
        .execute(
            "UPDATE artifact_state SET receipt = ?1 WHERE singleton = 1",
            [padded_receipt(&receipt).expect("padded receipt")],
        )
        .expect("seal the receipt");
    drop(connection);

    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &path,
        &receipt,
        &metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &ActiveControl,
    )
    .expect("open the sealed artifact");
    assert_eq!(
        reader.verified_artifact().clone_index_census(),
        &sealed_census
    );
    drop(reader);

    let connection = Connection::open(&path).expect("reopen for verification");
    let error = read_clone_index_census(&connection, &ActiveControl)
        .expect_err("verification recomputes the census and refuses the payload");
    assert!(
        matches!(error, CodeLexicalArtifactErrorV1::Corrupt(_)),
        "expected a corruption refusal, got {error:?}"
    );
}

#[test]
fn finalization_monitor_spawn_failure_is_typed_and_leaves_sqlite_reusable() {
    let mut connection = Connection::open_in_memory().expect("open artifact database");
    let _mutation_authority = create_mutable_test_schema(&connection);
    let transaction = connection.transaction().expect("start transaction");
    fail_next_finalization_monitor_spawn();
    let operation_ran = AtomicBool::new(false);
    let error = with_cancellable_sqlite_statement(&transaction, &ActiveControl, || {
        operation_ran.store(true, Ordering::SeqCst);
        Ok(())
    })
    .expect_err("injected monitor spawn failure must be typed");
    assert!(matches!(error, CodeLexicalArtifactErrorV1::Io(_)));
    assert!(
        !operation_ran.load(Ordering::SeqCst),
        "SQLite work must not start without its cancellation monitor"
    );
    with_cancellable_sqlite_statement(&transaction, &ActiveControl, || {
        transaction
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_error)
    })
    .expect("progress handler is cleared after spawn failure");
}

#[test]
fn posting_staging_keeps_source_page_order_without_a_serving_index() {
    let connection = Connection::open_in_memory().expect("open artifact database");
    let _mutation_authority = create_mutable_test_schema(&connection);
    for (page_ordinal, term) in [(0i64, "omega"), (0, "zeta"), (1, "alpha"), (1, "beta")] {
        connection
            .execute(
                "INSERT INTO term_posting_runs(page_ordinal, term, field, postings) VALUES (?1, ?2, 4, X'02')",
                params![page_ordinal, term],
            )
            .expect("seed page-ordered term run");
    }
    assert!(
        !table_exists(&connection, "ngram_posting_pages").expect("table probe"),
        "n-gram lists are rebuilt from rows and never staged"
    );
    for table in ["term_posting_runs", "exact_posting_runs", "row_chunk_pages"] {
        let secondary_indexes: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_index_list(?1) WHERE origin != 'pk'",
                [table],
                |row| row.get(0),
            )
            .expect("inspect staging indexes");
        assert_eq!(
            secondary_indexes, 0,
            "{table}: serving-key maintenance must remain absent during catch-up"
        );
    }

    let mut statement = connection
        .prepare(
            "SELECT page_ordinal, term FROM term_posting_runs ORDER BY page_ordinal, term, field",
        )
        .expect("prepare staging-order scan");
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .expect("scan staging order")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect staging order");
    assert_eq!(
        rows,
        [
            (0, "omega".to_owned()),
            (0, "zeta".to_owned()),
            (1, "alpha".to_owned()),
            (1, "beta".to_owned())
        ]
    );
    assert_eq!(
        statement.get_status(StatementStatus::Sort),
        0,
        "source-page catch-up must be the maintained table order"
    );
}

/// The parallel run merge yields the total order for plans of one run, a
/// ragged tail, and many runs. Plan keys are unique, as insert keys are.
#[test]
fn insert_plan_sort_matches_a_total_sort() {
    for length in [
        1u64,
        TERM_INSERT_CONTROL_INTERVAL as u64,
        TERM_INSERT_CONTROL_INTERVAL as u64 + 1,
        7 * TERM_INSERT_CONTROL_INTERVAL as u64 + 311,
    ] {
        let mut entries = (0..length)
            .map(|index| index.wrapping_mul(0x9e37_79b9_7f4a_7c15))
            .collect::<Vec<_>>();
        let mut expected = entries.clone();
        expected.sort_unstable();
        sort_insert_plan(&mut entries, |key| *key, &ActiveControl).expect("sort insert plan");
        assert_eq!(entries, expected, "plan of {length} entries");
    }
}

/// A pass that outgrows its memory sheds its highest keys and a later
/// pass rebuilds exactly them: the union of every pass equals one
/// unbounded pass, list for list, in key order, however the keys are
/// partitioned across workers and windows.
#[test]
fn bounded_ngram_passes_rebuild_exactly_the_unbounded_lists() {
    let postings = (0u32..600)
        .flat_map(|document| {
            (0i64..40)
                .filter(move |ngram| (document as i64 + ngram) % 3 != 0)
                .map(move |ngram| ((ngram % 2, ngram), document))
        })
        .collect::<Vec<_>>();
    let run = |memory_bytes: usize, partitions: usize, window_postings: usize| {
        let mut sealed = Vec::new();
        let mut passes = 0;
        let mut lower = None;
        loop {
            passes += 1;
            let mut pass = NgramListPassV1::new(lower, memory_bytes, partitions);
            for window in postings.chunks(window_postings) {
                let buckets = window
                    .chunks(97)
                    .map(|task| {
                        let mut buckets: NgramKeyBucketsV1 = vec![Vec::new(); partitions];
                        for &(key, document) in task {
                            buckets[ngram_partition(key, partitions)].push((key, document));
                        }
                        buckets
                    })
                    .collect::<Vec<_>>();
                pass.add_window(&buckets).expect("ascending documents");
            }
            let (lists, cutoff) = pass.finish().expect("seal pass");
            sealed.extend(lists);
            match cutoff {
                Some(cutoff) => lower = Some(cutoff),
                None => break,
            }
        }
        (sealed, passes)
    };
    let (unbounded, single) = run(usize::MAX, 1, postings.len());
    let (bounded, passes) = run(4 * 1024, 5, 512);
    assert_eq!(single, 1);
    assert!(
        passes > 2,
        "the bounded rebuild must actually spill: {passes} passes"
    );
    assert_eq!(bounded, unbounded);
    assert!(
        unbounded.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "sealed lists arrive in key order"
    );
}

fn encoded_postings(frequencies: bool, postings: &[(u32, u32)]) -> Vec<u8> {
    let mut encoder = PostingListEncoderV1::new(frequencies);
    for (document, frequency) in postings {
        encoder
            .push(*document, *frequency)
            .expect("ascending posting");
    }
    encoder.finish().expect("non-empty posting list")
}

fn decoded_postings(frequencies: bool, encoded: &[u8]) -> Vec<(u32, u32)> {
    PostingListDecoderV1::new(encoded, frequencies)
        .collect::<Result<Vec<_>, _>>()
        .expect("canonical posting list")
}

/// Finalization concatenates each key's page-ordered staging runs into
/// exactly one sealed list, drops the staging tables, freezes the
/// sealed tables, and serves every read from the clustered key alone.
#[test]
fn posting_merges_seal_one_list_per_serving_key() {
    let mut connection = Connection::open_in_memory().expect("open artifact database");
    let gate = register_builder_mutation_gate(&connection).expect("register builder mutation gate");
    create_schema(&connection).expect("create artifact schema");
    {
        let _authority = BuilderMutationGuardV1::enter(&gate).expect("enter authority");
        for (page_ordinal, term, field, postings) in [
            (0i64, "render", 4i64, vec![(1u32, 1u32), (2, 3)]),
            (0, "render", 7, vec![(4, 1)]),
            (0, "widget", 7, vec![(2, 1)]),
            (2, "render", 4, vec![(7, 2)]),
        ] {
            connection
                .execute(
                    "INSERT INTO term_posting_runs(page_ordinal, term, field, postings) VALUES (?1, ?2, ?3, ?4)",
                    params![page_ordinal, term, field, encoded_postings(true, &postings)],
                )
                .expect("seed term run");
        }
        for (page_ordinal, documents) in [(0i64, vec![(1u32, 1u32)]), (2, vec![(7, 1)])] {
            connection
                .execute(
                    "INSERT INTO exact_posting_runs(page_ordinal, term_id, field, documents) VALUES (?1, 9, 1, ?2)",
                    params![page_ordinal, encoded_postings(false, &documents)],
                )
                .expect("seed exact run");
        }
    }

    let transaction = connection.transaction().expect("merge transaction");
    let generation = test_metadata().generation;
    let authority = ServingIndexStepAuthorityV1 {
        mutation_gate: &gate,
        generation: &generation,
        ngram_memory_bytes: 1024 * 1024,
    };
    for ordinal in [1, 2, 3] {
        build_serving_index_step(&transaction, ordinal, &authority, &ActiveControl)
            .expect("merge posting family");
    }
    derive_statistics_step(&transaction, 1, &ActiveControl).expect("release staging pages");
    transaction.commit().expect("commit merges");

    for table in ["term_posting_runs", "exact_posting_runs"] {
        assert!(
            !table_exists(&connection, table).expect("table probe"),
            "{table} must not survive finalization"
        );
    }
    verify_builder_mutation_gate_schema(&connection)
        .expect("sealed layout verifies without the staging tables");
    let free_pages: i64 = connection
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .expect("read freelist");
    assert_eq!(
        free_pages, 0,
        "every page the dropped staging tables held leaves the file"
    );
    let ngram_lists: i64 = connection
        .query_row("SELECT COUNT(*) FROM ngram_postings", [], |row| row.get(0))
        .expect("count ngram lists");
    assert_eq!(ngram_lists, 0, "a corpus without rows seals no n-gram list");
    let terms = connection
        .prepare("SELECT term, in_fuzzy, lists FROM term_postings ORDER BY term")
        .expect("prepare term read")
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .expect("read term lists")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect term lists")
        .into_iter()
        .map(|(term, in_fuzzy, lists)| {
            let lists = term_lists_bytes(&lists).expect("stored term lists");
            let lists = decode_term_lists(&lists)
                .expect("canonical term lists")
                .into_iter()
                .map(|(field, frequency, postings)| {
                    (field, frequency, decoded_postings(true, postings))
                })
                .collect::<Vec<_>>();
            (term, in_fuzzy, lists)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        terms,
        [
            (
                "render".to_owned(),
                true,
                vec![(4, 3, vec![(1, 1), (2, 3), (7, 2)]), (7, 1, vec![(4, 1)])]
            ),
            // A term seen only as a subtoken is not fuzzy-eligible.
            ("widget".to_owned(), false, vec![(7, 1, vec![(2, 1)])]),
        ]
    );
    let exact: Vec<u8> = connection
        .query_row(
            "SELECT documents FROM exact_postings WHERE term_id = 9 AND field = 1",
            [],
            |row| row.get(0),
        )
        .expect("read exact list");
    assert_eq!(decoded_postings(false, &exact), [(1, 1), (7, 1)]);

    for table in ["term_postings", "exact_postings", "ngram_postings"] {
        let secondary_indexes: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_index_list(?1) WHERE origin != 'pk'",
                [table],
                |row| row.get(0),
            )
            .expect("inspect sealed indexes");
        assert_eq!(secondary_indexes, 0, "{table} carries no duplicate index");
    }
    let _authority = BuilderMutationGuardV1::enter(&gate).expect("enter authority");
    assert!(
        connection
            .execute(
                "INSERT INTO term_postings(term, in_fuzzy, lists) VALUES ('late', 1, X'040102')",
                [],
            )
            .is_err(),
        "sealed term postings must be frozen even under builder authority"
    );
    let plan = connection
        .prepare(
            "EXPLAIN QUERY PLAN SELECT documents, document_frequency FROM ngram_postings WHERE kind = ?1 AND ngram = ?2",
        )
        .expect("prepare ngram serving plan")
        .query_map(params![1i64, 10i64], |row| row.get::<_, String>(3))
        .expect("query ngram serving plan")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect ngram serving plan");
    assert!(
        plan.iter()
            .any(|detail| detail.contains("USING PRIMARY KEY")),
        "phrase candidates must seek the clustered key, got {plan:?}"
    );
}

#[test]
fn posting_merges_refuse_overlapping_staging() {
    for (runs, message) in [
        (
            [(0i64, vec![(1u32, 1u32), (4, 1)]), (1, vec![(3, 1)])],
            "overlap",
        ),
        ([(0, vec![(1, 1)]), (1, vec![(1, 1)])], "overlap"),
    ] {
        let mut connection = Connection::open_in_memory().expect("open artifact database");
        let gate = register_builder_mutation_gate(&connection).expect("register gate");
        create_schema(&connection).expect("create schema");
        {
            let _authority = BuilderMutationGuardV1::enter(&gate).expect("enter authority");
            for (page_ordinal, postings) in &runs {
                connection
                    .execute(
                        "INSERT INTO term_posting_runs(page_ordinal, term, field, postings) VALUES (?1, 'render', 4, ?2)",
                        params![page_ordinal, encoded_postings(true, postings)],
                    )
                    .expect("seed term run");
            }
        }
        let transaction = connection.transaction().expect("merge transaction");
        let generation = test_metadata().generation;
        let authority = ServingIndexStepAuthorityV1 {
            mutation_gate: &gate,
            generation: &generation,
            ngram_memory_bytes: 1024 * 1024,
        };
        let error = build_serving_index_step(&transaction, 1, &authority, &ActiveControl)
            .expect_err("out-of-order staging must fail closed");
        assert!(
            matches!(&error, CodeLexicalArtifactErrorV1::Corrupt(detail) if detail.contains(message)),
            "unexpected error: {error:?}"
        );
    }
}

#[test]
fn resume_refuses_the_superseded_ngram_staging_layout() {
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let path = directory.path().join("superseded-ngram-layout.sqlite");
    let metadata = test_metadata();
    drop(
        CodeLexicalArtifactBuilderV1::create(&path, metadata.clone())
            .expect("create current staging artifact"),
    );
    let connection = Connection::open(&path).expect("open staging artifact for fixture setup");
    connection
        .execute_batch(
            "ALTER TABLE ngram_postings RENAME TO current_ngram_postings;
             CREATE TABLE ngram_postings (
                kind INTEGER NOT NULL,
                ngram INTEGER NOT NULL,
                document_id INTEGER NOT NULL,
                PRIMARY KEY(kind, ngram, document_id)
             ) WITHOUT ROWID;
             DROP TABLE current_ngram_postings;",
        )
        .expect("install superseded branch-local layout");
    drop(connection);

    let error = match CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &path,
        metadata,
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &ActiveControl,
    ) {
        Ok(_) => panic!("resume must not mix superseded and current ngram layouts"),
        Err(error) => error,
    };
    assert!(matches!(error, CodeLexicalArtifactErrorV1::Incompatible(_)));
}

#[test]
fn persisted_progress_tail_seeks_latest_maintained_cursor_without_full_scan() {
    let connection = Connection::open_in_memory().expect("open progress database");
    connection
        .execute_batch(
            "CREATE TABLE source_page_cursors (
                page_ordinal INTEGER PRIMARY KEY,
                next_cursor BLOB NOT NULL
            );",
        )
        .expect("create source progress table");
    for page in 0..4_096i64 {
        connection
            .execute(
                "INSERT INTO source_page_cursors(page_ordinal, next_cursor) VALUES (?1, X'00')",
                [page],
            )
            .expect("seed persisted progress");
    }

    let mut statement = connection
        .prepare(PROGRESS_TAIL_QUERY)
        .expect("prepare maintained progress lookup");
    let latest: i64 = statement
        .query_row([], |row| row.get(0))
        .expect("read latest progress row");

    assert_eq!(latest, 4_095);
    assert_eq!(
        statement.get_status(StatementStatus::FullscanStep),
        0,
        "progress must seek the latest maintained cursor regardless of page count"
    );
    assert_eq!(
        statement.get_status(StatementStatus::Sort),
        0,
        "the primary-key tail lookup must not build a temporary order"
    );
}

/// Finalization adopts the base sections from their page receipts, so a
/// resumed digest wake walks only the source pages and the derived
/// statistics natively.
#[test]
fn bounded_finalization_resume_seeks_each_native_section_index() {
    let connection = Connection::open_in_memory().expect("open artifact database");
    let _mutation_authority = create_mutable_test_schema(&connection);
    connection
        .execute_batch(
            "INSERT INTO source_pages(page_ordinal, file_ordinal, chunk_count, payload_bytes, import_count, import_payload_bytes, import_digest, clone_body_count, ngram_digest, base_sections_receipt) VALUES (0, 0, 1, 1, 1, 1, 'imports', 0, 'ngrams', X'00');
             INSERT INTO field_stats(field, total_length) VALUES (1, 1);
             INSERT INTO term_postings(term, in_fuzzy, lists) VALUES ('term', 1, X'01010102');",
        )
        .expect("seed natively walked sections");

    for section in [
        FinalizationSectionV1::SourcePages,
        FinalizationSectionV1::FieldStatistics,
        FinalizationSectionV1::Vocabulary,
    ] {
        let plan = explain_native_seek_plan(&connection, section)
            .expect("explain bounded finalization resume query");
        assert!(
            plan.iter().any(|detail| detail.contains("SEARCH")),
            "{section:?} must seek an indexed native key, got {plan:?}"
        );
        assert!(
            plan.iter().all(|detail| !detail.contains("SCAN")),
            "{section:?} must not rescan the section on a resumed wake, got {plan:?}"
        );
        assert!(
            plan.iter().all(|detail| !detail.contains("TEMP B-TREE")),
            "{section:?} must not sort the section on a resumed wake, got {plan:?}"
        );
    }
}

fn explain_native_seek_plan(
    connection: &Connection,
    section: FinalizationSectionV1,
) -> Result<Vec<String>, CodeLexicalArtifactErrorV1> {
    let query = format!("EXPLAIN QUERY PLAN {}", section.walked_query(true)?);
    let mut statement = connection.prepare(&query).map_err(sqlite_error)?;
    let mut rows = match section {
        FinalizationSectionV1::SourcePages | FinalizationSectionV1::FieldStatistics => {
            statement.query(params![0i64, 1i64])
        }
        FinalizationSectionV1::Vocabulary => statement.query(params!["", 1i64]),
        other => panic!("{other:?} is not walked natively"),
    }
    .map_err(sqlite_error)?;
    let mut details = Vec::new();
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        details.push(row.get(3).map_err(sqlite_error)?);
    }
    Ok(details)
}

#[cfg(unix)]
#[test]
fn staging_open_refuses_a_symlink_even_when_its_target_is_private() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("private staging directory");
    let target = directory.path().join("target.sqlite");
    drop(create_private_file_retained(&target).expect("create private target"));
    let linked = directory.path().join("linked.sqlite");
    symlink(&target, &linked).expect("link private target");

    assert!(matches!(
        open_private_builder_connection(
            &linked,
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        ),
        Err(CodeLexicalArtifactErrorV1::Contract(_))
    ));
}

#[test]
fn staging_operations_refuse_same_name_replacement_after_open() {
    let directory = tempfile::tempdir().expect("private staging directory");
    let artifact = directory.path().join("artifact.sqlite");
    let replacement = directory.path().join("replacement.sqlite");
    let (connection, _retained, identity) = create_private_builder_connection(
        &artifact,
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
    )
    .expect("open artifact");
    drop(connection);
    drop(create_private_file_retained(&replacement).expect("create replacement"));
    std::fs::rename(&replacement, &artifact).expect("replace staged artifact");

    assert!(matches!(
        verify_staging_file_binding(&artifact, &identity),
        Err(CodeLexicalArtifactErrorV1::Corrupt(_))
    ));
}

#[test]
fn interrupted_staging_initialization_leaves_no_resumable_path() {
    let directory = tempfile::tempdir().expect("private staging directory");
    let staging = directory.path().join(".text-artifact-restart.staging");

    fail_next_staging_initialization();
    assert!(matches!(
        CodeLexicalArtifactBuilderV1::create(&staging, test_metadata()),
        Err(CodeLexicalArtifactErrorV1::Contract(_))
    ));
    // The staging path's existence is the resume signal, so it must never
    // appear without its singleton `artifact_state` row: a later
    // incarnation would read the empty row set as corruption and park a
    // projection that its sealed generation can still rederive.
    assert!(
        !staging.exists(),
        "staging path is visible without its committed state row"
    );

    drop(
        CodeLexicalArtifactBuilderV1::create(&staging, test_metadata())
            .expect("create a staging artifact over the interrupted attempt"),
    );
    drop(
        CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
            &staging,
            test_metadata(),
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
            &ActiveControl,
        )
        .expect("resume the published staging artifact"),
    );
}
