use std::collections::{BTreeMap, BTreeSet, VecDeque};

use tracedecay_store::{OperationPriorityV1, StoreClientIdV1, StoreOperationIdV1};

const FOREGROUND_WEIGHT: u32 = 4;
const BACKGROUND_WEIGHT: u32 = 1;
const DEFICIT_QUANTUM_BYTES: u64 = 64 * 1024;

pub(crate) trait QueueItem {
    fn operation_id(&self) -> &StoreOperationIdV1;
    fn client_id(&self) -> &StoreClientIdV1;
    fn priority(&self) -> OperationPriorityV1;
    fn admission_bytes(&self) -> u64;
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Key {
    client: StoreClientIdV1,
    priority: OperationPriorityV1,
}

impl Key {
    fn of(item: &impl QueueItem) -> Self {
        Self {
            client: item.client_id().clone(),
            priority: item.priority(),
        }
    }

    fn weight(&self) -> u32 {
        match self.priority {
            OperationPriorityV1::Health => 1,
            OperationPriorityV1::Foreground => FOREGROUND_WEIGHT,
            OperationPriorityV1::Background => BACKGROUND_WEIGHT,
        }
    }
}

struct ClientQueue<T> {
    items: VecDeque<T>,
    operation_deficit: u32,
    byte_deficit: u64,
}

impl<T> Default for ClientQueue<T> {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
            operation_deficit: 0,
            byte_deficit: 0,
        }
    }
}

impl<T> ClientQueue<T> {
    fn add_quantum(&mut self, weight: u32) {
        self.operation_deficit = self.operation_deficit.saturating_add(weight);
        self.byte_deficit = self
            .byte_deficit
            .saturating_add(DEFICIT_QUANTUM_BYTES.saturating_mul(u64::from(weight)));
    }
}

#[cfg(test)]
pub(crate) struct DispatchBatch<T> {
    pub(crate) priority: OperationPriorityV1,
    pub(crate) operations: Vec<T>,
}

#[cfg(test)]
pub(crate) enum Selection<T> {
    Batch(DispatchBatch<T>),
    Pending,
    Empty,
}

/// The only post-admission queue. Each entry is the complete accepted request,
/// including its reply channel and admission permit.
pub(crate) struct FairQueue<T> {
    health: VecDeque<T>,
    clients: BTreeMap<Key, ClientQueue<T>>,
    rotation: VecDeque<Key>,
    operation_ids: BTreeSet<StoreOperationIdV1>,
}

impl<T: QueueItem> Default for FairQueue<T> {
    fn default() -> Self {
        Self {
            health: VecDeque::new(),
            clients: BTreeMap::new(),
            rotation: VecDeque::new(),
            operation_ids: BTreeSet::new(),
        }
    }
}

impl<T: QueueItem> FairQueue<T> {
    pub(crate) fn push(&mut self, item: T) -> Result<(), T> {
        if !self.operation_ids.insert(item.operation_id().clone()) {
            return Err(item);
        }
        if item.priority() == OperationPriorityV1::Health {
            self.health.push_back(item);
            return Ok(());
        }
        let key = Key::of(&item);
        let queue = self.clients.entry(key.clone()).or_default();
        if queue.items.is_empty() {
            self.rotation.push_back(key);
        }
        queue.items.push_back(item);
        Ok(())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.operation_ids.is_empty()
    }

    pub(crate) fn drain_matching(&mut self, predicate: impl Fn(&T) -> bool) -> Vec<T> {
        let mut removed = Vec::new();
        drain_deque(&mut self.health, &predicate, &mut removed);
        let keys = self.clients.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let empty = {
                let queue = self.clients.get_mut(&key).expect("collected queue key");
                drain_deque(&mut queue.items, &predicate, &mut removed);
                queue.items.is_empty()
            };
            if empty {
                self.clients.remove(&key);
                self.rotation.retain(|candidate| candidate != &key);
            }
        }
        for item in &removed {
            self.operation_ids.remove(item.operation_id());
        }
        removed
    }

    pub(crate) fn drain_all(&mut self) -> Vec<T> {
        self.drain_matching(|_| true)
    }

    #[cfg(test)]
    pub(crate) fn next(&mut self, max_operations: u32, max_bytes: u64) -> Selection<T> {
        if let Some(operations) = select_fifo(
            &mut self.health,
            &mut self.operation_ids,
            max_operations,
            max_bytes,
        ) {
            return Selection::Batch(DispatchBatch {
                priority: OperationPriorityV1::Health,
                operations,
            });
        }
        let visits = self.rotation.len();
        for _ in 0..visits {
            let key = self.rotation.pop_front().expect("observed rotation entry");
            let (operations, empty) = {
                let queue = self.clients.get_mut(&key).expect("rotation queue exists");
                queue.add_quantum(key.weight());
                let mut operations = Vec::new();
                let mut bytes = 0_u64;
                while let Some(front) = queue.items.front() {
                    if queue.operation_deficit == 0
                        || queue.byte_deficit < front.admission_bytes()
                        || !fits(front, operations.len(), bytes, max_operations, max_bytes)
                    {
                        break;
                    }
                    let item = queue.items.pop_front().expect("front exists");
                    queue.operation_deficit -= 1;
                    queue.byte_deficit -= item.admission_bytes();
                    bytes += item.admission_bytes();
                    operations.push(item);
                }
                (operations, queue.items.is_empty())
            };
            if empty {
                self.clients.remove(&key);
            } else {
                self.rotation.push_back(key.clone());
            }
            if !operations.is_empty() {
                for item in &operations {
                    self.operation_ids.remove(item.operation_id());
                }
                return Selection::Batch(DispatchBatch {
                    priority: key.priority,
                    operations,
                });
            }
        }
        if self.operation_ids.is_empty() {
            Selection::Empty
        } else {
            Selection::Pending
        }
    }

    /// Removes every currently queued complete request in fair dispatch order.
    /// Batch policy deliberately lives in the writer, not in admission.
    pub(crate) fn drain_fair(&mut self) -> Vec<T> {
        let mut selected = Vec::with_capacity(self.operation_ids.len());
        while let Some(item) = self.health.pop_front() {
            self.operation_ids.remove(item.operation_id());
            selected.push(item);
        }

        while !self.rotation.is_empty() {
            let visits = self.rotation.len();
            let mut progressed = false;
            for _ in 0..visits {
                let key = self.rotation.pop_front().expect("observed rotation entry");
                let (items, empty) = {
                    let queue = self.clients.get_mut(&key).expect("rotation queue exists");
                    queue.add_quantum(key.weight());
                    let mut items = Vec::new();
                    while let Some(front) = queue.items.front() {
                        if queue.operation_deficit == 0
                            || queue.byte_deficit < front.admission_bytes()
                        {
                            break;
                        }
                        let item = queue.items.pop_front().expect("front exists");
                        queue.operation_deficit -= 1;
                        queue.byte_deficit -= item.admission_bytes();
                        items.push(item);
                    }
                    (items, queue.items.is_empty())
                };
                if empty {
                    self.clients.remove(&key);
                } else {
                    self.rotation.push_back(key.clone());
                }
                if !items.is_empty() {
                    progressed = true;
                    for item in &items {
                        self.operation_ids.remove(item.operation_id());
                    }
                    selected.extend(items);
                }
            }
            debug_assert!(progressed || !self.rotation.is_empty());
        }
        debug_assert!(self.operation_ids.is_empty());
        selected
    }
}

fn drain_deque<T>(queue: &mut VecDeque<T>, predicate: &impl Fn(&T) -> bool, removed: &mut Vec<T>) {
    let mut retained = VecDeque::with_capacity(queue.len());
    while let Some(item) = queue.pop_front() {
        if predicate(&item) {
            removed.push(item);
        } else {
            retained.push_back(item);
        }
    }
    *queue = retained;
}

#[cfg(test)]
fn select_fifo<T: QueueItem>(
    queue: &mut VecDeque<T>,
    operation_ids: &mut BTreeSet<StoreOperationIdV1>,
    max_operations: u32,
    max_bytes: u64,
) -> Option<Vec<T>> {
    let mut selected = Vec::new();
    let mut bytes = 0_u64;
    while let Some(front) = queue.front() {
        if !fits(front, selected.len(), bytes, max_operations, max_bytes) {
            break;
        }
        let item = queue.pop_front().expect("front exists");
        bytes += item.admission_bytes();
        operation_ids.remove(item.operation_id());
        selected.push(item);
    }
    (!selected.is_empty()).then_some(selected)
}

#[cfg(test)]
fn fits(
    item: &impl QueueItem,
    selected: usize,
    bytes: u64,
    max_operations: u32,
    max_bytes: u64,
) -> bool {
    u32::try_from(selected)
        .ok()
        .and_then(|count| count.checked_add(1))
        .is_some_and(|count| count <= max_operations)
        && bytes
            .checked_add(item.admission_bytes())
            .is_some_and(|total| total <= max_bytes)
}
