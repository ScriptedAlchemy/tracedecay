mod common;

use tracedecay_application::{
    OperationReceipt, StreamEvent, StreamEventKind, StreamFrontier, StreamGap, StreamTermination,
    validate_stream,
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

    assert!(validate_stream(&events).is_err());
}

#[test]
fn stream_rejects_an_invalid_gap_event() {
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
        StreamEvent {
            sequence: 0,
            kind: StreamEventKind::<()>::Gap(StreamGap {
                first_missing_sequence: 4,
                last_missing_sequence: 3,
                frontier: StreamFrontier {
                    next_sequence: 5,
                    retained_from_sequence: 0,
                    resume_token: None,
                },
            }),
        },
        StreamEvent::terminal(1, StreamTermination::completed(receipt)).unwrap(),
    ];

    assert!(validate_stream(&events).is_err());
}
