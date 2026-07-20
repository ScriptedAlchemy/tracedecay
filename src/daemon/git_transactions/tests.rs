use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use tracedecay_domain::RepositoryId;

use super::queue::RepositoryMutationQueue;

#[test]
fn same_repository_mutations_never_enter_the_native_section_together() {
    let queue = Arc::new(RepositoryMutationQueue::default());
    let repository = RepositoryId::new("repository.fixture").expect("repository id");
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let workers: Vec<_> = (0..4)
        .map(|_| {
            let queue = Arc::clone(&queue);
            let repository = repository.clone();
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            thread::spawn(move || {
                queue
                    .with_repository(&repository, || {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(current, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(5));
                        active.fetch_sub(1, Ordering::SeqCst);
                    })
                    .expect("queue is available");
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("worker joins");
    }

    assert_eq!(peak.load(Ordering::SeqCst), 1);
}
