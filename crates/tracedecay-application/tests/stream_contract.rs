mod common;

use tracedecay_application::{
    OperationReceipt, StreamEvent, StreamEventKind, StreamFrontier, StreamGap, StreamTermination,
    StreamValidationError, validate_stream,
};
use tracedecay_domain::UtcMicros;

#[test]
fn stream_is_ordered_and_has_exactly_one_terminal_event() {
    let operation = common::operation();
    let context = common::context(&operation);
    let receipt = OperationReceipt::completed(
        UtcMicros(2),
        UtcMicros(3),
        context.deadline().clone(),
        Default::default(),
    )
    .unwrap();
    let events = vec![
        StreamEvent::item(0, "first").unwrap(),
        StreamEvent::terminal(1, StreamTermination::completed(receipt)).unwrap(),
    ];

    validate_stream(&events).unwrap();
}

#[test]
fn stream_rejects_events_after_the_terminal_receipt() {
    let operation = common::operation();
    let context = common::context(&operation);
    let receipt = OperationReceipt::completed(
        UtcMicros(2),
        UtcMicros(3),
        context.deadline().clone(),
        Default::default(),
    )
    .unwrap();
    let events = vec![
        StreamEvent::terminal(0, StreamTermination::completed(receipt)).unwrap(),
        StreamEvent::item(1, "late").unwrap(),
    ];

    assert_eq!(
        validate_stream(&events),
        Err(StreamValidationError::EventAfterTerminal)
    );
}

#[test]
fn stream_rejects_multiple_terminal_receipts() {
    let operation = common::operation();
    let context = common::context(&operation);
    let receipt = OperationReceipt::completed(
        UtcMicros(2),
        UtcMicros(3),
        context.deadline().clone(),
        Default::default(),
    )
    .unwrap();
    let events = vec![
        StreamEvent::<()>::terminal(0, StreamTermination::completed(receipt.clone())).unwrap(),
        StreamEvent::terminal(1, StreamTermination::completed(receipt)).unwrap(),
    ];

    assert_eq!(
        validate_stream(&events),
        Err(StreamValidationError::MultipleTerminalEvents)
    );
}

#[test]
fn stream_rejects_an_invalid_gap_event() {
    let events = [StreamEvent {
        sequence: 4,
        kind: StreamEventKind::<()>::Gap(StreamGap {
            first_missing_sequence: 4,
            last_missing_sequence: 3,
            frontier: StreamFrontier {
                next_sequence: 5,
                retained_from_sequence: 0,
                resume_token: None,
            },
        }),
    }];

    assert_eq!(
        validate_stream(&events),
        Err(StreamValidationError::InvalidGap(
            "stream gap has an invalid range".to_owned()
        ))
    );
}

#[test]
fn stream_gap_advances_sequence_to_the_end_of_the_missing_range() {
    let operation = common::operation();
    let context = common::context(&operation);
    let receipt = OperationReceipt::completed(
        UtcMicros(2),
        UtcMicros(3),
        context.deadline().clone(),
        Default::default(),
    )
    .unwrap();
    let events = vec![
        StreamEvent::item(2, ()).unwrap(),
        StreamEvent {
            sequence: 3,
            kind: StreamEventKind::Gap(StreamGap {
                first_missing_sequence: 3,
                last_missing_sequence: 5,
                frontier: StreamFrontier {
                    next_sequence: 9,
                    retained_from_sequence: 6,
                    resume_token: None,
                },
            }),
        },
        StreamEvent::item(6, ()).unwrap(),
        StreamEvent::item(7, ()).unwrap(),
        StreamEvent::terminal(8, StreamTermination::completed(receipt)).unwrap(),
    ];

    assert_eq!(validate_stream(&events), Ok(()));
}

#[test]
fn stream_gap_sequence_must_equal_its_first_missing_sequence() {
    let events = [StreamEvent {
        sequence: 3,
        kind: StreamEventKind::<()>::Gap(StreamGap {
            first_missing_sequence: 4,
            last_missing_sequence: 5,
            frontier: StreamFrontier {
                next_sequence: 6,
                retained_from_sequence: 6,
                resume_token: None,
            },
        }),
    }];

    assert!(matches!(
        validate_stream(&events),
        Err(StreamValidationError::InvalidGap(_))
    ));
}

#[test]
fn stream_gap_sequence_overflow_is_typed() {
    let events = [StreamEvent {
        sequence: u64::MAX,
        kind: StreamEventKind::<()>::Gap(StreamGap {
            first_missing_sequence: u64::MAX,
            last_missing_sequence: u64::MAX,
            frontier: StreamFrontier {
                next_sequence: u64::MAX,
                retained_from_sequence: u64::MAX,
                resume_token: None,
            },
        }),
    }];

    assert_eq!(
        validate_stream(&events),
        Err(StreamValidationError::SequenceOverflow)
    );
}

#[test]
fn stream_item_sequence_overflow_is_typed() {
    let events = [StreamEvent::item(u64::MAX, ()).unwrap()];

    assert_eq!(
        validate_stream(&events),
        Err(StreamValidationError::SequenceOverflow)
    );
}

#[test]
fn stream_accepts_a_terminal_event_at_the_maximum_sequence() {
    let operation = common::operation();
    let context = common::context(&operation);
    let receipt = OperationReceipt::completed(
        UtcMicros(2),
        UtcMicros(3),
        context.deadline().clone(),
        Default::default(),
    )
    .unwrap();
    let events =
        [StreamEvent::<()>::terminal(u64::MAX, StreamTermination::completed(receipt)).unwrap()];

    assert_eq!(validate_stream(&events), Ok(()));
}
