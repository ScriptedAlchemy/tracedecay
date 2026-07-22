use tracedecay_rusqlite_runtime::effects::{
    EffectCoordinator, EffectDispatchOutcome, SqliteOriginEffectTransactions,
    SqliteTargetEffectTransactions,
};
use tracedecay_store::{RuntimeSubmitOutcomeV1, StoreEffectIdV1};

use crate::cutover_support::{
    Probe, RecordingEffect, TestDatabase, fixture, outbox_request, run, writer, writer_locator,
    writer_with_executor,
};

#[test]
fn restart_replays_writer_receipt_and_effect_inbox_acknowledgement() {
    let fixture = fixture().s9;
    let origin = TestDatabase::new("s9-origin.sqlite3");
    let target = TestDatabase::new("s9-target.sqlite3");
    let request = outbox_request(
        &fixture.origin_binding,
        &fixture.target_binding,
        "operation.cutover.restart",
        &fixture.effect_id,
        &fixture.ordering_key,
    );

    let first_writer = writer(&origin, &fixture.origin_binding);
    let first_receipt = match run(first_writer.submit(request.clone(), Probe::for_submit(&request)))
        .expect("commit S9 origin write")
    {
        RuntimeSubmitOutcomeV1::Committed { receipt } => receipt,
        outcome => panic!("expected initial commit, got {outcome:?}"),
    };
    first_writer
        .shutdown_and_join()
        .expect("close first S9 writer");

    let restarted_writer = writer(&origin, &fixture.origin_binding);
    assert_eq!(
        run(restarted_writer.submit(request.clone(), Probe::for_submit(&request),))
            .expect("replay S9 origin write"),
        RuntimeSubmitOutcomeV1::ExactReplay {
            receipt: first_receipt
        }
    );
    let effect_id = StoreEffectIdV1::new(fixture.effect_id).expect("valid S9 effect identity");
    let target_locator = writer_locator(&target, &fixture.target_binding);
    let target_writer = writer_with_executor(&target, &fixture.target_binding, RecordingEffect);
    let first_acknowledgement = run(async {
        let mut origin_transactions = SqliteOriginEffectTransactions::open(&restarted_writer)
            .expect("open read-only origin effect view");
        let mut target_transactions = SqliteTargetEffectTransactions::new(&target_writer);
        match EffectCoordinator
            .dispatch(
                &effect_id,
                &fixture.origin_binding,
                &fixture.target_binding,
                &mut origin_transactions,
                &mut target_transactions,
            )
            .await
        {
            Ok(EffectDispatchOutcome::Acknowledged {
                receipt,
                replayed: false,
            }) => *receipt,
            outcome => panic!("expected first inbox application and acknowledgement: {outcome:?}"),
        }
    });
    restarted_writer
        .shutdown_and_join()
        .expect("close acknowledged S9 writer");
    target_writer
        .shutdown_and_join()
        .expect("close first S9 target writer");

    let restarted_writer = writer(&origin, &fixture.origin_binding);
    let target_writer = tracedecay_rusqlite_runtime::PersistentWriter::start(
        target_locator,
        tracedecay_store::AdmissionConfigV1::default(),
        RecordingEffect,
    )
    .expect("restart S9 target writer");
    let replayed_acknowledgement = run(async {
        let mut origin_transactions = SqliteOriginEffectTransactions::open(&restarted_writer)
            .expect("reopen read-only origin effect view");
        let mut target_transactions = SqliteTargetEffectTransactions::new(&target_writer);
        match EffectCoordinator
            .dispatch(
                &effect_id,
                &fixture.origin_binding,
                &fixture.target_binding,
                &mut origin_transactions,
                &mut target_transactions,
            )
            .await
        {
            Ok(EffectDispatchOutcome::Acknowledged {
                receipt,
                replayed: true,
            }) => *receipt,
            outcome => panic!("expected durable acknowledgement replay after restart: {outcome:?}"),
        }
    });
    restarted_writer
        .shutdown_and_join()
        .expect("close replayed S9 writer");
    target_writer
        .shutdown_and_join()
        .expect("close replayed S9 target writer");
    assert_eq!(replayed_acknowledgement, first_acknowledgement);
}

#[test]
fn replay_pass_is_bounded_and_resumes_remaining_outbox_work() {
    let fixture = fixture().s9;
    let origin = TestDatabase::new("s9-bounded-origin.sqlite3");
    let target = TestDatabase::new("s9-bounded-target.sqlite3");
    let first = outbox_request(
        &fixture.origin_binding,
        &fixture.target_binding,
        "operation.cutover.bounded.first",
        "effect.cutover.bounded.first",
        "project.cutover.origin.bounded.first",
    );
    let second = outbox_request(
        &fixture.origin_binding,
        &fixture.target_binding,
        "operation.cutover.bounded.second",
        "effect.cutover.bounded.second",
        "project.cutover.origin.bounded.second",
    );
    let origin_writer = writer(&origin, &fixture.origin_binding);
    for request in [&first, &second] {
        assert!(matches!(
            run(origin_writer.submit(request.clone(), Probe::for_submit(request)))
                .expect("commit bounded S9 outbox work"),
            RuntimeSubmitOutcomeV1::Committed { .. }
        ));
    }
    let target_writer = writer_with_executor(&target, &fixture.target_binding, RecordingEffect);
    run(async {
        let mut origin_transactions = SqliteOriginEffectTransactions::open(&origin_writer)
            .expect("open bounded S9 origin effect view");
        let mut target_transactions = SqliteTargetEffectTransactions::new(&target_writer);

        let first_pass = EffectCoordinator
            .replay_bounded(
                &fixture.origin_binding,
                &fixture.target_binding,
                &mut origin_transactions,
                &mut target_transactions,
                1,
            )
            .await
            .expect("run first bounded S9 replay pass");
        assert_eq!(first_pass.attempts.len(), 1);
        assert!(matches!(
            &first_pass.attempts[0].result,
            Ok(EffectDispatchOutcome::Acknowledged { .. })
        ));

        let second_pass = EffectCoordinator
            .replay_bounded(
                &fixture.origin_binding,
                &fixture.target_binding,
                &mut origin_transactions,
                &mut target_transactions,
                1,
            )
            .await
            .expect("resume bounded S9 replay");
        assert_eq!(second_pass.attempts.len(), 1);
        assert!(matches!(
            &second_pass.attempts[0].result,
            Ok(EffectDispatchOutcome::Acknowledged { .. })
        ));
    });
    origin_writer
        .shutdown_and_join()
        .expect("close bounded S9 origin writer");
    target_writer
        .shutdown_and_join()
        .expect("close bounded S9 target writer");

    let applied: i64 = target
        .connect()
        .query_row("SELECT COUNT(*) FROM cutover_effects", [], |row| row.get(0))
        .expect("count bounded S9 effects");
    assert_eq!(applied, 2);
}
