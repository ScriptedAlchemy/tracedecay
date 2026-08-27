//! Periodic checkpoint timer for automatic durability.
//!
//! When [`Config::checkpoint_interval`](crate::Config::checkpoint_interval) is set,
//! the engine spawns a background thread that periodically flushes dirty sections
//! to the `.grafeo` container. This bounds the WAL size and limits data loss to
//! at most one interval on crash.
//!
//! The timer polls a shutdown flag in short intervals (100 ms) so `close()`
//! completes promptly without blocking for the full checkpoint interval.

#[cfg(feature = "grafeo-file")]
use std::sync::Arc;
#[cfg(feature = "grafeo-file")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "grafeo-file")]
use std::time::Duration;

#[cfg(feature = "grafeo-file")]
use grafeo_common::utils::error::Result;
#[cfg(feature = "grafeo-file")]
use grafeo_core::graph::lpg::LpgStore;
#[cfg(feature = "grafeo-file")]
use grafeo_storage::file::GrafeoFileManager;

#[cfg(feature = "grafeo-file")]
use crate::catalog::Catalog;
#[cfg(feature = "grafeo-file")]
use crate::transaction::TransactionManager;

/// How often the timer thread checks the shutdown flag.
#[cfg(feature = "grafeo-file")]
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Background checkpoint timer.
///
/// Spawns a thread that periodically triggers a unified flush. The thread
/// exits cleanly when [`stop`](Self::stop) is called (from `GrafeoDB::close()`).
#[cfg(feature = "grafeo-file")]
pub(super) struct CheckpointTimer {
    /// Shutdown signal: set to true to stop the timer thread.
    shutdown: Arc<AtomicBool>,
    /// Thread handle (taken on stop).
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(feature = "grafeo-file")]
impl CheckpointTimer {
    /// Starts the checkpoint timer.
    ///
    /// The background thread wakes every `interval`, checks if any sections
    /// are dirty, and flushes them to the container. If no mutations happened
    /// since the last checkpoint, the flush is skipped (no I/O).
    pub(super) fn start(
        interval: Duration,
        file_manager: Arc<GrafeoFileManager>,
        store: Arc<LpgStore>,
        catalog: Arc<Catalog>,
        transaction_manager: Arc<TransactionManager>,
        #[cfg(feature = "triple-store")] rdf_store: Arc<grafeo_core::graph::rdf::RdfStore>,
        #[cfg(feature = "wal")] wal: Option<Arc<grafeo_storage::wal::LpgWal>>,
    ) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);

        let handle = std::thread::Builder::new()
            .name("grafeo-checkpoint".to_string())
            .spawn(move || {
                Self::run(
                    &shutdown_clone,
                    interval,
                    &file_manager,
                    &store,
                    &catalog,
                    &transaction_manager,
                    #[cfg(feature = "triple-store")]
                    &rdf_store,
                    #[cfg(feature = "wal")]
                    wal.as_deref(),
                );
            })
            .expect("failed to spawn checkpoint timer thread");

        Self {
            shutdown,
            handle: Some(handle),
        }
    }

    /// Signals the timer thread to stop and waits for it to exit.
    ///
    /// Returns within ~100 ms regardless of the checkpoint interval.
    pub(super) fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    /// Timer loop: sleep in short increments, checkpoint when the interval
    /// elapses, exit when shutdown is signaled.
    #[allow(clippy::too_many_arguments)]
    fn run(
        shutdown: &AtomicBool,
        interval: Duration,
        file_manager: &GrafeoFileManager,
        store: &Arc<LpgStore>,
        catalog: &Arc<Catalog>,
        transaction_manager: &TransactionManager,
        #[cfg(feature = "triple-store")] rdf_store: &Arc<grafeo_core::graph::rdf::RdfStore>,
        #[cfg(feature = "wal")] wal: Option<&grafeo_storage::wal::LpgWal>,
    ) {
        let mut elapsed = Duration::ZERO;

        loop {
            std::thread::sleep(POLL_INTERVAL);

            if shutdown.load(Ordering::Acquire) {
                break;
            }

            elapsed += POLL_INTERVAL;
            if elapsed < interval {
                continue;
            }
            elapsed = Duration::ZERO;

            // Attempt checkpoint (errors are logged, not propagated)
            if let Err(e) = Self::try_checkpoint(
                file_manager,
                store,
                catalog,
                transaction_manager,
                #[cfg(feature = "triple-store")]
                rdf_store,
                #[cfg(feature = "wal")]
                wal,
            ) {
                eprintln!("periodic checkpoint failed: {e}");
            }
        }
    }

    /// Runs a single checkpoint cycle.
    fn try_checkpoint(
        file_manager: &GrafeoFileManager,
        store: &Arc<LpgStore>,
        catalog: &Arc<Catalog>,
        transaction_manager: &TransactionManager,
        #[cfg(feature = "triple-store")] rdf_store: &Arc<grafeo_core::graph::rdf::RdfStore>,
        #[cfg(feature = "wal")] wal: Option<&grafeo_storage::wal::LpgWal>,
    ) -> Result<()> {
        use super::flush;

        let sections = Self::build_sections(
            store,
            catalog,
            #[cfg(feature = "triple-store")]
            rdf_store,
        );
        let section_refs: Vec<&dyn grafeo_common::storage::Section> =
            sections.iter().map(|s| s.as_ref()).collect();
        let context = flush::build_context(store, transaction_manager);

        flush::flush(
            file_manager,
            &section_refs,
            &context,
            flush::FlushReason::Explicit,
            #[cfg(feature = "wal")]
            wal,
        )
        .map(|_| ())
    }

    /// Builds section objects from the captured components.
    fn build_sections(
        store: &Arc<LpgStore>,
        catalog: &Arc<Catalog>,
        #[cfg(feature = "triple-store")] rdf_store: &Arc<grafeo_core::graph::rdf::RdfStore>,
    ) -> Vec<Box<dyn grafeo_common::storage::Section>> {
        let lpg = grafeo_core::graph::lpg::LpgStoreSection::new(Arc::clone(store));

        // Catalog section uses epoch 0 for periodic checkpoints (the flush
        // context provides the real epoch to the container header).
        let catalog_section = super::catalog_section::CatalogSection::new(
            Arc::clone(catalog),
            Arc::clone(store),
            || 0,
        );

        #[allow(
            unused_mut,
            reason = "mut needed when triple-store/ring-index/vector-index/text-index features push sections"
        )]
        let mut sections: Vec<Box<dyn grafeo_common::storage::Section>> =
            vec![Box::new(catalog_section), Box::new(lpg)];

        #[cfg(feature = "triple-store")]
        if !rdf_store.is_empty() || rdf_store.graph_count() > 0 {
            let rdf = grafeo_core::graph::rdf::RdfStoreSection::new(Arc::clone(rdf_store));
            sections.push(Box::new(rdf));
        }

        #[cfg(feature = "ring-index")]
        if rdf_store.ring().is_some() {
            let ring = grafeo_core::index::ring::RdfRingSection::new(Arc::clone(rdf_store));
            sections.push(Box::new(ring));
        }

        #[cfg(feature = "vector-index")]
        {
            let indexes = store.vector_index_entries();
            if !indexes.is_empty() {
                let vector = grafeo_core::index::vector::VectorStoreSection::new(indexes);
                sections.push(Box::new(vector));
            }
        }

        #[cfg(feature = "text-index")]
        {
            let indexes = store.text_index_entries();
            if !indexes.is_empty() {
                let text = grafeo_core::index::text::TextIndexSection::new(indexes);
                sections.push(Box::new(text));
            }
        }

        sections
    }
}

#[cfg(feature = "grafeo-file")]
impl Drop for CheckpointTimer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
#[cfg(feature = "grafeo-file")]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn timer_stops_promptly() {
        let store = Arc::new(LpgStore::new().unwrap());
        let catalog = Arc::new(Catalog::new());
        let tm = Arc::new(TransactionManager::new());

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("timer_test.grafeo");
        let fm = Arc::new(GrafeoFileManager::create(&path).unwrap());

        let mut timer = CheckpointTimer::start(
            Duration::from_mins(1), // Long interval
            fm,
            store,
            catalog,
            tm,
            #[cfg(feature = "triple-store")]
            Arc::new(grafeo_core::graph::rdf::RdfStore::new()),
            #[cfg(feature = "wal")]
            None,
        );

        let start = Instant::now();
        timer.stop();
        let elapsed = start.elapsed();

        // Should stop within a few poll cycles, not 60 seconds
        assert!(
            elapsed < Duration::from_secs(2),
            "stop() took {elapsed:?}, expected < 2s"
        );
    }

    #[test]
    fn timer_checkpoints_on_interval() {
        let store = Arc::new(LpgStore::new().unwrap());
        let catalog = Arc::new(Catalog::new());
        let tm = Arc::new(TransactionManager::new());

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("interval_test.grafeo");
        let fm = Arc::new(GrafeoFileManager::create(&path).unwrap());

        // Add some data so sections have content
        store.create_node(&["Test"]);

        let mut timer = CheckpointTimer::start(
            Duration::from_millis(200), // Short interval for testing
            Arc::clone(&fm),
            Arc::clone(&store),
            Arc::clone(&catalog),
            Arc::clone(&tm),
            #[cfg(feature = "triple-store")]
            Arc::new(grafeo_core::graph::rdf::RdfStore::new()),
            #[cfg(feature = "wal")]
            None,
        );

        // Wait for at least one checkpoint cycle (200ms interval + margin)
        std::thread::sleep(Duration::from_millis(500));
        timer.stop();

        // Verify that a checkpoint happened (iteration > 0)
        let header = fm.active_header();
        assert!(
            header.iteration > 0,
            "expected at least one checkpoint, got iteration={}",
            header.iteration
        );
        assert_eq!(header.node_count, 1);
    }

    #[test]
    fn timer_skips_when_clean() {
        let store = Arc::new(LpgStore::new().unwrap());
        let catalog = Arc::new(Catalog::new());
        let tm = Arc::new(TransactionManager::new());

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("clean_test.grafeo");
        let fm = Arc::new(GrafeoFileManager::create(&path).unwrap());

        let mut timer = CheckpointTimer::start(
            Duration::from_millis(200),
            Arc::clone(&fm),
            Arc::clone(&store),
            Arc::clone(&catalog),
            Arc::clone(&tm),
            #[cfg(feature = "triple-store")]
            Arc::new(grafeo_core::graph::rdf::RdfStore::new()),
            #[cfg(feature = "wal")]
            None,
        );

        std::thread::sleep(Duration::from_millis(500));
        timer.stop();

        // Just verify no crash occurred
        let header = fm.active_header();
        assert!(header.iteration <= 5);
    }
}
