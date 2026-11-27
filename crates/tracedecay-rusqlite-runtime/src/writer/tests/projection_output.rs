use super::*;
use crate::{
    StorageOperationError, StorageOperationOutput, test_support::projection_batch_request,
};

struct ProjectionOutputExecutor(StorageOperationOutput);

impl StorageOperationExecutor for ProjectionOutputExecutor {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        savepoint.execute_batch(
            "CREATE TABLE projection_output_marker (value INTEGER NOT NULL);
             INSERT INTO projection_output_marker VALUES (1)",
        )
    }

    fn execute_closed(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> Result<StorageOperationOutput, StorageOperationError> {
        self.execute(savepoint, payload)?;
        Ok(self.0)
    }
}

struct CancelAtOuterCommit(Probe);

impl RuntimeRequestProbeV1 for CancelAtOuterCommit {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        self.0.cancellation_identity()
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        self.0.deadline_identity()
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        self.0.interruption()
    }

    fn try_begin_commit(&self) -> bool {
        self.0.interruption.store(1, Ordering::SeqCst);
        false
    }
}

fn marker_table_count(database: &TestDatabase) -> i64 {
    Connection::open(&database.0)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'projection_output_marker'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn a_rolled_back_outer_transaction_never_reports_projected_heads() {
    let database = TestDatabase::new();
    let request = projection_batch_request(6);
    let writer = start_with_executor(
        &database,
        &request,
        ProjectionOutputExecutor(StorageOperationOutput::ExternalSourceProjection { projected: 6 }),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let cancelled = runtime
        .block_on(writer.submit(
            request.clone(),
            Arc::new(CancelAtOuterCommit(Probe::new(&request, None))),
        ))
        .unwrap();
    assert!(matches!(
        cancelled,
        RuntimeSubmitOutcomeV1::CancelledBeforeCommit { .. }
    ));
    assert_eq!(
        marker_table_count(&database),
        0,
        "the released savepoint was rolled back"
    );

    let retry = runtime
        .block_on(writer.submit(request.clone(), Arc::new(Probe::new(&request, None))))
        .unwrap();
    retry.validate_for(&request).unwrap();
    assert!(matches!(
        retry,
        RuntimeSubmitOutcomeV1::ExternalSourceProjectionCommitted {
            projected: 6,
            cancellation: None,
            ..
        }
    ));
    assert_eq!(marker_table_count(&database), 1);
    writer.shutdown_and_join().unwrap();
}

#[test]
fn an_invalid_projection_output_rolls_back_before_the_commit() {
    for output in [
        StorageOperationOutput::None,
        StorageOperationOutput::ExternalSourceProjection { projected: 7 },
    ] {
        let database = TestDatabase::new();
        let request = projection_batch_request(6);
        let writer = start_with_executor(&database, &request, ProjectionOutputExecutor(output));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let result =
            runtime.block_on(writer.submit(request.clone(), Arc::new(Probe::new(&request, None))));
        assert!(
            matches!(result, Err(WriterActorError::StorageFailure(_))),
            "{result:?}"
        );
        assert_eq!(marker_table_count(&database), 0);
        writer.shutdown_and_join().unwrap();
    }
}
