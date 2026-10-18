use super::*;
use tracedecay_contracts::remote::discovery::RemoteAuthorityDiscoveryV1;

const NODE_SECRET: [u8; 32] = [9; 32];

/// A store on a profile whose own Brain differs from the one its identity
/// row serves: the shape an operator join leaves behind.
fn joined_fixture() -> Fixture {
    let mut binding = remote_test_binding();
    binding.shard_id.brain_id = BrainId::new("brain.laptop").unwrap();
    fixture_with_binding(binding)
}

fn discovery() -> RemoteAuthorityDiscoveryV1 {
    let writer = writer();
    let (_, enrollment) = enrollment_record(&NODE_SECRET);
    RemoteAuthorityDiscoveryV1 {
        policy: replay_policy(&writer.scope),
        writer,
        enrollment,
    }
}

#[test]
fn authority_discovery_answers_only_after_the_writer_is_published() {
    let fixture = fixture();
    let storage = storage(&fixture);
    let (enrollment, receipt) = enrollment_record(&NODE_SECRET);
    insert_enrollment(&fixture, &enrollment, &receipt);

    let (state, answer) = storage
        .authority_discovery(receipt.clone(), UtcMicros(20))
        .unwrap();
    assert!(matches!(
        state,
        CurrentRemoteAuthorityStateV1::Unavailable {
            reason:
                tracedecay_domain::RemoteAuthorityUnavailableReasonV1::WriterAuthorityUnpublished,
            ..
        }
    ));
    assert_eq!(answer, None);

    let writer = writer();
    publish_writer(&storage, &writer);
    let (state, answer) = storage.authority_discovery(receipt, UtcMicros(20)).unwrap();
    assert_eq!(
        state,
        CurrentRemoteAuthorityStateV1::Available(writer.authority.clone())
    );
    assert_eq!(answer, Some(discovery()));
}

#[test]
fn a_joined_store_adopts_the_authority_enrollment_verbatim_and_converges() {
    let fixture = joined_fixture();
    let joined = storage(&fixture);
    assert!(joined.is_joined());
    assert_eq!(joined.member_brain().as_str(), "brain.remote");
    assert!(!storage(&super::fixture()).is_joined());

    let discovery = discovery();
    for attempt in ["join", "rerun"] {
        joined
            .record_authority_discovery(&discovery, UtcMicros(20))
            .unwrap_or_else(|error| panic!("{attempt}: {error}"));
    }
    let fingerprint = RemoteCredentialFingerprintV1::from_secret(&NODE_SECRET).unwrap();
    assert!(matches!(
        joined
            .credential_by_fingerprint(RemoteCredentialClassV1::Enrollment, &fingerprint)
            .unwrap(),
        RemoteCredentialAuthorityRecordV1::Enrollment { enrollment, receipt }
            if *enrollment == discovery.enrollment.enrollment && *receipt == discovery.enrollment
    ));
    let registrations = joined.credential_registrations(8).unwrap();
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].brain_id, *joined.member_brain());
    assert_eq!(
        <RemoteSqliteStorageV1 as RemoteCapturePortV1>::current_writer_authority(
            &joined,
            &discovery.writer
        )
        .unwrap(),
        CurrentRemoteAuthorityStateV1::Available(discovery.writer.authority.clone()),
        "capture admission reads the authority's writer as observed at join"
    );
}

#[test]
fn a_joined_store_refuses_a_rival_enrollment_a_rolled_back_writer_and_a_foreign_brain() {
    let fixture = joined_fixture();
    let joined = storage(&fixture);
    let discovery = discovery();
    joined
        .record_authority_discovery(&discovery, UtcMicros(20))
        .unwrap();

    let mut rival = discovery.clone();
    rival.enrollment.enrollment.expires_at = UtcMicros(90);
    assert!(matches!(
        joined.record_authority_discovery(&rival, UtcMicros(21)),
        Err(RemoteSqliteStorageErrorV1::Conflict)
    ));

    let mut stale = discovery.clone();
    stale.writer.authority.fence.authority_epoch = tracedecay_domain::AuthorityEpoch(10);
    assert!(matches!(
        joined.record_authority_discovery(&stale, UtcMicros(21)),
        Err(RemoteSqliteStorageErrorV1::Conflict)
    ));

    let mut foreign = discovery;
    foreign.enrollment.enrollment.brain_id = BrainId::new("brain.other").unwrap();
    assert!(matches!(
        joined.record_authority_discovery(&foreign, UtcMicros(21)),
        Err(RemoteSqliteStorageErrorV1::BindingMismatch)
    ));
}
