use super::*;

#[test]
fn queued_fact_write_rechecks_authority_before_opening_a_transaction() {
    let database = TestDatabase::new();
    let request = fact_request("operation.authority.queued", "key.authority.queued", 'q');
    let applied = Arc::new(AtomicU64::new(0));
    let writer = start(&database, &request, Arc::clone(&applied));
    let authority = Arc::new(RevokeAfterAdmissionAuthority {
        admitted: AtomicBool::new(false),
    });
    let probe = Arc::new(Probe::new(&request, None));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    let queued_outcome = runtime
        .block_on(writer.submit_authorized(request, probe, authority))
        .unwrap();

    assert_eq!(
        queued_outcome,
        RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::MissingAuthority,
        }
    );
    assert_eq!(applied.load(Ordering::SeqCst), 0);
    let table_count: i64 = Connection::open(&database.0)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'writer_test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 0);
    writer.shutdown_and_join().unwrap();
}

#[test]
fn queued_evidence_and_anchor_writes_recheck_authority_before_sql_dispatch() {
    let evidence = RepositoryWritePayloadV1::EvidenceAssembly(Box::new(
        crate::repository::evidence_assembly::tests::write_fixture("authority.test"),
    ));
    let anchor = RepositoryWritePayloadV1::RetrievalAnchorDisposition(Box::new(
        RetrievalAnchorDispositionRecordV1::new(
            "disposition.authority.fixture",
            tracedecay_domain::RetrievalAnchorId::new("retrieval.source.fixture").unwrap(),
            FactOwnerV1::Project {
                project_id: ProjectId::new("project.fixture").unwrap(),
            },
            AnchorDispositionStateV1::Unavailable,
            None,
            AnchorDispositionReasonClassV1::SourceUnavailable,
            UtcMicros(1),
        )
        .unwrap(),
    ));

    for (label, payload, digest_byte) in [
        ("evidence", evidence, 'e'),
        ("retrieval_anchor", anchor, 'r'),
    ] {
        let database = TestDatabase::new();
        let request = project_fixture_request(
            &format!("operation.authority.{label}"),
            &format!("key.authority.{label}"),
            digest_byte,
            payload,
        );
        let applied = Arc::new(AtomicU64::new(0));
        let writer = start(&database, &request, Arc::clone(&applied));
        let authority = Arc::new(RevokeAfterAdmissionAuthority {
            admitted: AtomicBool::new(false),
        });
        let probe = Arc::new(Probe::new(&request, None));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let outcome = runtime
            .block_on(writer.submit_authorized(request, probe, authority))
            .unwrap();

        assert_eq!(
            outcome,
            RuntimeSubmitOutcomeV1::Unavailable {
                reason: UnavailableReasonV1::MissingAuthority,
            },
            "{label} write bypassed the actor authority recheck"
        );
        assert_eq!(applied.load(Ordering::SeqCst), 0);
        let table_count: i64 = Connection::open(&database.0)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'writer_test'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 0);
        writer.shutdown_and_join().unwrap();
    }
}

#[test]
fn fact_write_rechecks_authority_before_outer_commit_and_rolls_back() {
    let database = TestDatabase::new();
    let request = fact_request(
        "operation.authority.precommit",
        "key.authority.precommit",
        'p',
    );
    let applied = Arc::new(AtomicU64::new(0));
    let allowed = Arc::new(AtomicBool::new(true));
    let writer = start_with_persistence(
        &database,
        &request,
        Box::new(RevokingPersistence {
            inner: TestPersistence {
                applied: Arc::clone(&applied),
                sequence: 0,
            },
            allowed: Arc::clone(&allowed),
        }),
    );
    let authority = Arc::new(ToggleAuthority { allowed });
    let probe = Arc::new(Probe::new(&request, None));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    let outcome = runtime
        .block_on(writer.submit_authorized(request, probe, authority))
        .unwrap();

    assert_eq!(
        outcome,
        RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::MissingAuthority,
        }
    );
    assert_eq!(applied.load(Ordering::SeqCst), 1);
    let table_count: i64 = Connection::open(&database.0)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'writer_test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 0);
    writer.shutdown_and_join().unwrap();
}
