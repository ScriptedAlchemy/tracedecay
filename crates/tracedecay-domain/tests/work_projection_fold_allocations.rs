//! The Work projection fold must cost one command-set insertion per event,
//! not a copy of the whole growing state. A counting allocator measures the
//! rebuild alone: the history is built first, no JSON is decoded, and only the
//! fold runs while the counter is armed.
//!
//! This binary holds exactly one test because the counter is process-global.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeSet;
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, TaskId, UtcMicros, WorkAuthority, WorkEvent,
    WorkEventKind, WorkProjectionStateV1, WorkVersion, WorktreeId,
};

struct CountingAllocator;

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

impl CountingAllocator {
    fn record(layout: Layout) {
        if TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record(layout);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::record(layout);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record(Layout::from_size_align(new_size, layout.align()).unwrap_or(layout));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

/// A valid history of `len` events with distinct command identities: one
/// creation followed by dependency replans, which the reducer accepts at any
/// version.
fn history(len: u64) -> Vec<WorkEvent> {
    let authority = WorkAuthority::new(
        id::<ProjectId>("project.work.fold-cost"),
        id::<RepositoryId>("repository.work.fold-cost"),
        id::<WorktreeId>("worktree.work.fold-cost"),
        id::<ActorId>("actor.work.fold-cost"),
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
    )
    .unwrap();
    let digest = ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap();
    (1..=len)
        .map(|version| {
            let kind = if version == 1 {
                WorkEventKind::Created {
                    title: "measure the fold".to_owned(),
                    dependencies: BTreeSet::new(),
                }
            } else {
                WorkEventKind::DependenciesReplanned {
                    dependencies: BTreeSet::from([id::<TaskId>("task.work.fold-cost.dependency")]),
                }
            };
            WorkEvent::new(
                id::<TaskId>("task.work.fold-cost"),
                WorkVersion::new(version).unwrap(),
                authority.clone(),
                UtcMicros(i64::try_from(version).unwrap()),
                id(&format!("command.work.fold-cost.{version:05}")),
                digest.clone(),
                kind,
            )
            .unwrap()
        })
        .collect()
}

/// Allocation count and bytes for one full rebuild of `history`.
fn measure_rebuild(history: &[WorkEvent]) -> (usize, usize) {
    // Warm anything lazily initialised so only the fold is measured.
    black_box(WorkProjectionStateV1::rebuild(history).unwrap());
    ALLOCATIONS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
    let state = WorkProjectionStateV1::rebuild(black_box(history));
    TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);
    let state = state.unwrap();
    assert_eq!(state.command_ids().len(), history.len());
    black_box(state);
    (
        ALLOCATIONS.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed),
    )
}

#[test]
fn rebuilding_a_long_history_allocates_linearly_in_its_length() {
    let short = history(1_024);
    let long = history(2_048);
    let (short_allocations, short_bytes) = measure_rebuild(&short);
    let (long_allocations, long_bytes) = measure_rebuild(&long);
    eprintln!(
        "rebuild of {} events: {short_allocations} allocations / {short_bytes} bytes; \
         rebuild of {} events: {long_allocations} allocations / {long_bytes} bytes",
        short.len(),
        long.len()
    );

    // Each event may insert its command identity and replace the dependency
    // set: a handful of allocations per event, never a copy of every earlier
    // command. Copying the growing set costs on the order of len²/2 entries,
    // which is hundreds of allocations per event at these lengths.
    for (events, allocations) in [
        (short.len(), short_allocations),
        (long.len(), long_allocations),
    ] {
        assert!(
            allocations <= events * 4,
            "rebuilding {events} events took {allocations} allocations; \
             a linear fold stays within four per event"
        );
    }
    // Doubling the history must about double the cost, not quadruple it.
    assert!(
        long_allocations < short_allocations * 3,
        "allocations grew from {short_allocations} to {long_allocations} when the history \
         doubled; a linear fold grows about twofold"
    );
    assert!(
        long_bytes < short_bytes * 3,
        "allocated bytes grew from {short_bytes} to {long_bytes} when the history doubled"
    );
}
