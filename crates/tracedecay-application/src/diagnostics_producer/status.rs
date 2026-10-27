//! Compiler status retained by one primitive runtime, with one current producer.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tokio::sync::{Mutex, MutexGuard};
use tracedecay_domain::CodeGenerationId;

/// Outcome of one producer run over one project root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilerProducerRunV1 {
    Published {
        generation: CodeGenerationId,
        inserted: u64,
        unresolved: usize,
        /// Projects tsc checked for this generation.
        checked: usize,
        /// Projects whose previous result this generation republished.
        reused: usize,
    },
    /// The compiler reported findings, but none resolved onto a file the
    /// code-index generation knows, so nothing was published.
    NoResolvableDiagnostics {
        unresolved: Vec<String>,
    },
    /// The code index has no complete generation for this root yet.
    CodeIndexGenerationUnavailable,
    /// No project could be checked: every compiler failed (spawn failure, a
    /// tsconfig naming no inputs, a crash), or none is installed.
    CompilerFailed {
        reason: String,
    },
    PublicationFailed {
        reason: String,
    },
    /// A newer producer was appointed before this run entered publication.
    Superseded,
}

/// How one tsconfig fared in the most recent run. A project with no compiler
/// is not recorded: reads resolve that from the filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeScriptProjectCheckV1 {
    /// Its findings are part of the published snapshot.
    Checked,
    Failed {
        reason: String,
    },
}

/// The latest producer outcome of one project owner: the run and how each
/// tsconfig fared in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeScriptProducerStatusV1 {
    pub(super) run: CompilerProducerRunV1,
    pub(super) checks: BTreeMap<PathBuf, TypeScriptProjectCheckV1>,
}

impl TypeScriptProducerStatusV1 {
    #[must_use]
    pub fn new(
        run: CompilerProducerRunV1,
        checks: BTreeMap<PathBuf, TypeScriptProjectCheckV1>,
    ) -> Self {
        Self { run, checks }
    }

    #[must_use]
    pub fn run(&self) -> &CompilerProducerRunV1 {
        &self.run
    }
}

/// Read view of the status retained by one primitive runtime. Reopening its
/// route preserves the last completed check and appoints a new producer; an
/// older task cannot overwrite it while that task's server is being retired.
#[derive(Clone, Debug, Default)]
pub struct TypeScriptProducerStatusHandleV1 {
    status: Arc<RwLock<ProducerStatus>>,
    publication: Arc<Mutex<()>>,
}

#[derive(Debug, Default)]
struct ProducerStatus {
    producer: Option<Arc<()>>,
    latest: Option<Arc<TypeScriptProducerStatusV1>>,
}

impl TypeScriptProducerStatusHandleV1 {
    /// Claim publication before spawning the producer, so task polling order
    /// cannot let an older producer claim after its replacement.
    pub(super) fn start_producer(&self) -> TypeScriptProducerStatusWriterV1 {
        let producer = Arc::new(());
        self.status
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .producer = Some(Arc::clone(&producer));
        TypeScriptProducerStatusWriterV1 {
            status: self.clone(),
            producer,
        }
    }

    /// The most recent completed status from an admitted producer, if any.
    #[must_use]
    pub fn current(&self) -> Option<Arc<TypeScriptProducerStatusV1>> {
        self.status
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latest
            .clone()
    }
}

/// Publication authority for one producer instance, not for a pathname.
pub(super) struct TypeScriptProducerStatusWriterV1 {
    status: TypeScriptProducerStatusHandleV1,
    producer: Arc<()>,
}

impl TypeScriptProducerStatusWriterV1 {
    /// Compiler work may overlap. Publication is ordered for this retained
    /// runtime: an already-admitted old publication finishes before the new
    /// one, and an old producer entering after replacement cannot publish.
    pub(super) async fn begin_publication(&self) -> Option<TypeScriptProducerPublicationV1<'_>> {
        let gate = self.status.publication.lock().await;
        let current = self
            .status
            .status
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .producer
            .as_ref()
            .is_some_and(|producer| Arc::ptr_eq(producer, &self.producer));
        current.then_some(TypeScriptProducerPublicationV1 {
            writer: self,
            _gate: gate,
        })
    }

    fn publish(&self, snapshot: TypeScriptProducerStatusV1) {
        let mut status = self
            .status
            .status
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The authority check and replacement share this lock. A new claim
        // cannot interleave between accepting this producer and its write.
        if status
            .producer
            .as_ref()
            .is_some_and(|producer| Arc::ptr_eq(producer, &self.producer))
        {
            status.latest = Some(Arc::new(snapshot));
        }
    }
}

/// Held across the record transaction and the matching status publication.
pub(super) struct TypeScriptProducerPublicationV1<'a> {
    writer: &'a TypeScriptProducerStatusWriterV1,
    _gate: MutexGuard<'a, ()>,
}

impl TypeScriptProducerPublicationV1<'_> {
    pub(super) fn publish(self, snapshot: TypeScriptProducerStatusV1) {
        self.writer.publish(snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompilerProducerRunV1, TypeScriptProducerStatusHandleV1, TypeScriptProducerStatusV1,
    };
    use std::collections::BTreeMap;

    fn failed(reason: &str) -> TypeScriptProducerStatusV1 {
        TypeScriptProducerStatusV1::new(
            CompilerProducerRunV1::CompilerFailed {
                reason: reason.to_owned(),
            },
            BTreeMap::new(),
        )
    }

    #[test]
    fn a_status_handle_publishes_to_its_owner_only() {
        let owner = TypeScriptProducerStatusHandleV1::default();
        assert_eq!(owner.current(), None);
        owner.start_producer().publish(failed("owner"));
        assert_eq!(owner.current().as_deref(), Some(&failed("owner")));
        assert_eq!(TypeScriptProducerStatusHandleV1::default().current(), None);
    }

    #[test]
    fn a_replacement_claim_fences_old_runs_before_the_replacement_is_polled() {
        let owner = TypeScriptProducerStatusHandleV1::default();
        let predecessor = owner.start_producer();
        predecessor.publish(failed("last completed check"));
        let sibling = TypeScriptProducerStatusHandleV1::default();
        let sibling_producer = sibling.start_producer();
        sibling_producer.publish(failed("sibling"));

        let replacement = owner.start_producer();
        predecessor.publish(failed("late old check"));
        assert_eq!(
            owner.current().as_deref(),
            Some(&failed("last completed check")),
            "a superseded task is fenced even before the new task runs"
        );
        replacement.publish(failed("replacement"));
        predecessor.publish(failed("another late old check"));
        assert_eq!(owner.current().as_deref(), Some(&failed("replacement")));
        sibling_producer.publish(failed("sibling next check"));
        assert_eq!(
            sibling.current().as_deref(),
            Some(&failed("sibling next check")),
            "claims in one retained runtime do not fence a linked sibling"
        );
    }

    #[tokio::test]
    async fn an_entered_publication_finishes_before_its_replacement_and_does_not_gate_a_sibling() {
        let owner = TypeScriptProducerStatusHandleV1::default();
        let predecessor = owner.start_producer();
        let old_publication = predecessor.begin_publication().await.unwrap();
        let replacement = owner.start_producer();
        let mut next_publication = Box::pin(replacement.begin_publication());
        std::future::poll_fn(|cx| {
            assert!(next_publication.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        let sibling = TypeScriptProducerStatusHandleV1::default();
        sibling
            .start_producer()
            .begin_publication()
            .await
            .unwrap()
            .publish(failed("sibling"));
        assert_eq!(sibling.current().as_deref(), Some(&failed("sibling")));

        old_publication.publish(failed("old record transaction completed"));
        assert_eq!(owner.current(), None, "a new claim fences the old status");
        next_publication
            .await
            .unwrap()
            .publish(failed("replacement"));
        assert_eq!(owner.current().as_deref(), Some(&failed("replacement")));
        assert!(
            predecessor.begin_publication().await.is_none(),
            "the superseded producer must not enter another record transaction"
        );
    }
}
