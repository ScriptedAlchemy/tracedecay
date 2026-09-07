use proptest::{prelude::*, test_runner::Config as ProptestConfig};
use tracedecay_store::{StoreClientIdV1, StoreOperationIdV1, StoreOperationMetadataV1};

use super::*;

const TEST_QUANTUM_BYTES: u64 = 64 * 1024;

fn limits(operations: u32, bytes: u64) -> Limits {
    Limits::new(
        Capacity { operations, bytes },
        Capacity { operations, bytes },
        bytes,
        bytes,
    )
    .unwrap()
}

fn metadata(bytes: u64, priority: OperationPriorityV1) -> StoreOperationMetadataV1 {
    let mut metadata = crate::test_support::metadata("operation.admission", "key.admission", 'a');
    metadata.admission_bytes = bytes;
    metadata.priority = priority;
    metadata
}

#[derive(Clone, Debug)]
struct Item {
    operation: StoreOperationIdV1,
    client: StoreClientIdV1,
    priority: OperationPriorityV1,
    bytes: u64,
}

impl QueueItem for Item {
    fn operation_id(&self) -> &StoreOperationIdV1 {
        &self.operation
    }

    fn client_id(&self) -> &StoreClientIdV1 {
        &self.client
    }

    fn priority(&self) -> OperationPriorityV1 {
        self.priority
    }

    fn admission_bytes(&self) -> u64 {
        self.bytes
    }
}

fn item(index: usize, client: usize, priority: OperationPriorityV1, bytes: u64) -> Item {
    Item {
        operation: StoreOperationIdV1::new(format!("operation.{index}")).unwrap(),
        client: StoreClientIdV1::new(format!("client.{client}")).unwrap(),
        priority,
        bytes,
    }
}

fn priority(value: u8) -> OperationPriorityV1 {
    match value % 3 {
        0 => OperationPriorityV1::Health,
        1 => OperationPriorityV1::Foreground,
        _ => OperationPriorityV1::Background,
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn admission_obeys_exact_operation_and_byte_caps(
        operation_cap in 1_u32..9,
        byte_cap in 1_u64..4096,
    ) {
        let operation_admission = Admission::new(limits(operation_cap, u64::from(operation_cap)));
        let one_byte = metadata(1, OperationPriorityV1::Foreground);
        let permits = (0..operation_cap)
            .map(|_| operation_admission.reserve(&one_byte).unwrap())
            .collect::<Vec<_>>();

        prop_assert_eq!(
            operation_admission.usage(Lane::General),
            Usage { operations: operation_cap, bytes: u64::from(operation_cap) }
        );
        prop_assert_eq!(
            operation_admission.reserve(&one_byte).unwrap_err(),
            SaturationScopeV1::ShardOperations
        );
        drop(permits);

        let byte_admission = Admission::new(limits(2, byte_cap));
        prop_assert_eq!(
            byte_admission
                .reserve(&metadata(byte_cap + 1, OperationPriorityV1::Foreground))
                .unwrap_err(),
            SaturationScopeV1::ShardBytes
        );
        let exact = byte_admission
            .reserve(&metadata(byte_cap, OperationPriorityV1::Foreground))
            .unwrap();
        prop_assert_eq!(byte_admission.usage(Lane::General).bytes, byte_cap);
        prop_assert_eq!(
            byte_admission.reserve(&one_byte).unwrap_err(),
            SaturationScopeV1::ShardBytes
        );
        drop(exact);
        prop_assert_eq!(byte_admission.usage(Lane::General), Usage::default());
    }

    #[test]
    fn permit_release_conserves_operations_and_bytes(
        reservations in prop::collection::vec((1_u64..1024, any::<bool>()), 1..16),
    ) {
        let total_bytes = reservations.iter().map(|(bytes, _)| bytes).sum();
        let admission = Admission::new(limits(reservations.len() as u32, total_bytes));
        let mut permits = reservations
            .iter()
            .map(|(bytes, _)| {
                Some(admission.reserve(&metadata(*bytes, OperationPriorityV1::Foreground)).unwrap())
            })
            .collect::<Vec<_>>();

        for (permit, (_, release)) in permits.iter_mut().zip(&reservations) {
            if *release {
                drop(permit.take());
            }
        }
        let expected = Usage {
            operations: reservations.iter().filter(|(_, release)| !release).count() as u32,
            bytes: reservations
                .iter()
                .filter(|(_, release)| !release)
                .map(|(bytes, _)| bytes)
                .sum(),
        };
        prop_assert_eq!(admission.usage(Lane::General), expected);

        drop(permits);
        prop_assert_eq!(admission.usage(Lane::General), Usage::default());
    }

    #[test]
    fn fair_drain_preserves_every_operation_id_exactly_once(
        entries in prop::collection::vec((0_usize..6, 0_u8..3, 1_u64..=TEST_QUANTUM_BYTES * 2), 1..24),
    ) {
        let mut queue = FairQueue::default();
        for (index, (client, priority_value, bytes)) in entries.iter().copied().enumerate() {
            let queued = item(index, client, priority(priority_value), bytes);
            queue.push(queued.clone()).unwrap();
            prop_assert!(queue.push(queued).is_err());
        }

        let mut actual = queue
            .drain_fair()
            .into_iter()
            .map(|item| item.operation.as_str().to_owned())
            .collect::<Vec<_>>();
        actual.sort();
        let mut expected = (0..entries.len())
            .map(|index| format!("operation.{index}"))
            .collect::<Vec<_>>();
        expected.sort();

        prop_assert_eq!(actual, expected);
        prop_assert!(queue.is_empty());
    }

    #[test]
    fn health_dispatches_before_generated_general_work(
        general_count in 1_usize..12,
        health_count in 1_usize..12,
    ) {
        let mut queue = FairQueue::default();
        for index in 0..general_count {
            queue.push(item(index, index % 4, OperationPriorityV1::Foreground, 1)).unwrap();
        }
        for offset in 0..health_count {
            queue.push(item(general_count + offset, offset % 4, OperationPriorityV1::Health, 1)).unwrap();
        }

        let selection = queue.next(health_count as u32, health_count as u64);
        prop_assert!(matches!(&selection, Selection::Batch(_)), "health batch should be dispatchable");
        let Selection::Batch(batch) = selection else {
            unreachable!("selection was asserted to be a batch")
        };
        prop_assert_eq!(batch.priority, OperationPriorityV1::Health);
        prop_assert_eq!(batch.operations.len(), health_count);
        prop_assert!(batch.operations.iter().all(|item| item.priority == OperationPriorityV1::Health));
    }

    #[test]
    fn every_dispatch_batch_stays_within_both_limits(
        (max_operations, max_bytes, costs) in (1_u32..8, 1_u64..2048).prop_flat_map(
            |(max_operations, max_bytes)| (
                Just(max_operations),
                Just(max_bytes),
                prop::collection::vec(1_u64..=max_bytes, 1..24),
            )
        ),
    ) {
        let mut queue = FairQueue::default();
        for (index, bytes) in costs.iter().copied().enumerate() {
            queue.push(item(index, 0, OperationPriorityV1::Foreground, bytes)).unwrap();
        }

        while !queue.is_empty() {
            match queue.next(max_operations, max_bytes) {
                Selection::Batch(batch) => {
                    prop_assert!(batch.operations.len() <= max_operations as usize);
                    prop_assert!(batch.operations.iter().map(|item| item.bytes).sum::<u64>() <= max_bytes);
                }
                Selection::Pending => {}
                Selection::Empty => prop_assert!(queue.is_empty(), "nonempty queue reported empty"),
            }
        }
    }

    #[test]
    fn generated_clients_receive_bounded_wdrr_service(
        entries in prop::collection::vec((any::<bool>(), 1_u64..=TEST_QUANTUM_BYTES * 2), 1..9),
    ) {
        let mut queue = FairQueue::default();
        for (index, (foreground, bytes)) in entries.iter().copied().enumerate() {
            let priority = if foreground {
                OperationPriorityV1::Foreground
            } else {
                OperationPriorityV1::Background
            };
            queue.push(item(index, index, priority, bytes)).unwrap();
        }

        let max_rounds = entries
            .iter()
            .map(|(foreground, bytes)| {
                let weight = if *foreground { 4 } else { 1 };
                bytes.div_ceil(TEST_QUANTUM_BYTES * weight)
            })
            .max()
            .unwrap();
        let call_bound = entries.len() as u64 * (max_rounds + 1);
        let mut serviced = std::collections::BTreeSet::new();

        for _ in 0..call_bound {
            match queue.next(1, TEST_QUANTUM_BYTES * 2) {
                Selection::Batch(batch) => {
                    prop_assert_eq!(batch.operations.len(), 1);
                    serviced.insert(batch.operations[0].operation.as_str().to_owned());
                }
                Selection::Pending => {}
                Selection::Empty => break,
            }
            if queue.is_empty() {
                break;
            }
        }

        prop_assert!(queue.is_empty(), "generated client starved past {call_bound} dispatch calls");
        prop_assert_eq!(serviced.len(), entries.len());
    }
}

#[test]
fn weighted_round_services_exactly_four_foreground_items_then_one_background_item() {
    let mut queue = FairQueue::default();
    for index in 0..4 {
        queue
            .push(item(index, 0, OperationPriorityV1::Foreground, 1))
            .unwrap();
    }
    queue
        .push(item(4, 1, OperationPriorityV1::Background, 1))
        .unwrap();

    let Selection::Batch(foreground) = queue.next(8, 8) else {
        panic!("foreground batch")
    };
    let Selection::Batch(background) = queue.next(8, 8) else {
        panic!("background batch")
    };
    assert_eq!(foreground.priority, OperationPriorityV1::Foreground);
    assert_eq!(foreground.operations.len(), 4);
    assert_eq!(background.priority, OperationPriorityV1::Background);
    assert_eq!(background.operations.len(), 1);
}
