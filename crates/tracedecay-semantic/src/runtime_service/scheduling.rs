type SemanticRuntimePrepareFutureV1 = Pin<
    Box<
        dyn Future<
                Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>,
            > + Send
            + 'static,
    >,
>;
type SemanticRuntimeCommitFutureV1 = Pin<
    Box<
        dyn Future<Output = Result<SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1>>
            + Send
            + 'static,
    >,
>;
type SemanticRuntimePublishedFutureV1 = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type SemanticRuntimeInstallV1 = Box<
    dyn FnOnce(&SemanticGenerationPointerV1) -> Result<(), SemanticRuntimeScheduleFailureV1>
        + Send
        + 'static,
>;
type SemanticRuntimePublishedV1 = Box<
    dyn FnOnce(SemanticGenerationPointerV1) -> SemanticRuntimePublishedFutureV1 + Send + 'static,
>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticGenerationPointerV1 {
    pub generation: VectorGenerationIdV1,
    pub source_generation: CodeGenerationId,
    pub projection_key: ProjectionKeyV1,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRuntimeScheduleFailureV1 {
    Artifact,
    Runtime,
    Projection,
    Publication,
    Cancelled,
    DeadlineExceeded,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SemanticRuntimeScheduleStatusV1 {
    Unavailable,
    Indexing {
        target_generation: CodeGenerationId,
        target_projection_key: Option<ProjectionKeyV1>,
        completed_units: u64,
        total_units: u64,
        prior_generation: Option<VectorGenerationIdV1>,
    },
    Current {
        generation: VectorGenerationIdV1,
    },
    Failed {
        reason: SemanticRuntimeScheduleFailureV1,
        prior_generation: Option<VectorGenerationIdV1>,
    },
}

pub struct SemanticRuntimeScheduleCancellationV1 {
    cancelled: AtomicBool,
    completed_units: AtomicU64,
    total_units: u64,
    linked: Option<Arc<dyn crate::semantic_evaluation::SemanticEvaluationCancellationV1>>,
}

impl std::fmt::Debug for SemanticRuntimeScheduleCancellationV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SemanticRuntimeScheduleCancellationV1")
            .field("cancelled", &self.cancelled.load(Ordering::Acquire))
            .field(
                "completed_units",
                &self.completed_units.load(Ordering::Acquire),
            )
            .field("total_units", &self.total_units)
            .field("linked", &self.linked.is_some())
            .finish()
    }
}

impl SemanticRuntimeScheduleCancellationV1 {
    pub fn new(total_units: u64) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            completed_units: AtomicU64::new(0),
            total_units,
            linked: None,
        }
    }

    pub fn new_linked(
        total_units: u64,
        linked: Arc<dyn crate::semantic_evaluation::SemanticEvaluationCancellationV1>,
    ) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            completed_units: AtomicU64::new(0),
            total_units,
            linked: Some(linked),
        }
    }

    pub fn cancelled(&self) -> bool {
        self.interruption().is_some()
    }

    pub fn failure(&self) -> Option<SemanticRuntimeScheduleFailureV1> {
        self.interruption().map(|interruption| match interruption {
            SemanticExecutionInterruptionV1::Cancelled => {
                SemanticRuntimeScheduleFailureV1::Cancelled
            }
            SemanticExecutionInterruptionV1::DeadlineExceeded => {
                SemanticRuntimeScheduleFailureV1::DeadlineExceeded
            }
        })
    }

    pub fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
        if self.cancelled.load(Ordering::Acquire) {
            return Some(SemanticExecutionInterruptionV1::Cancelled);
        }
        self.linked
            .as_ref()
            .and_then(|linked| linked.interruption())
    }

    pub fn set_completed_units(&self, completed_units: u64) {
        self.completed_units
            .fetch_max(completed_units.min(self.total_units), Ordering::AcqRel);
    }

    pub(crate) fn completed_units(&self) -> u64 {
        self.completed_units.load(Ordering::Acquire)
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl SemanticExecutionAuthority for SemanticRuntimeScheduleCancellationV1 {
    fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
        Self::interruption(self)
    }
}

pub struct PreparedSemanticRuntimeCommitV1 {
    commit: Box<dyn FnOnce() -> SemanticRuntimeCommitFutureV1 + Send + 'static>,
    install: Option<SemanticRuntimeInstallV1>,
    published: Option<SemanticRuntimePublishedV1>,
}

impl PreparedSemanticRuntimeCommitV1 {
    pub fn new<Commit, CommitFuture>(commit: Commit) -> Self
    where
        Commit: FnOnce() -> CommitFuture + Send + 'static,
        CommitFuture: Future<Output = Result<SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1>>
            + Send
            + 'static,
    {
        Self {
            commit: Box::new(move || Box::pin(commit())),
            install: None,
            published: None,
        }
    }

    async fn commit(
        self,
    ) -> Result<
        (
            SemanticGenerationPointerV1,
            Option<SemanticRuntimeInstallV1>,
            Option<SemanticRuntimePublishedV1>,
        ),
        SemanticRuntimeScheduleFailureV1,
    > {
        let Self {
            commit,
            install,
            published,
        } = self;
        commit().await.map(|pointer| (pointer, install, published))
    }

    pub fn on_success<Install>(mut self, install: Install) -> Self
    where
        Install: FnOnce(&SemanticGenerationPointerV1) -> Result<(), SemanticRuntimeScheduleFailureV1>
            + Send
            + 'static,
    {
        self.install = Some(Box::new(install));
        self
    }

    /// Observe only after the query runtime and scheduler pointer are installed.
    pub fn on_published<Published, PublishedFuture>(mut self, published: Published) -> Self
    where
        Published: FnOnce(SemanticGenerationPointerV1) -> PublishedFuture + Send + 'static,
        PublishedFuture: Future<Output = ()> + Send + 'static,
    {
        self.published = Some(Box::new(move |pointer| Box::pin(published(pointer))));
        self
    }
}

pub struct SemanticRuntimeWorkV1 {
    target_generation: CodeGenerationId,
    target_projection_key: Option<ProjectionKeyV1>,
    total_units: u64,
    prepare: Box<
        dyn FnOnce(Arc<SemanticRuntimeScheduleCancellationV1>) -> SemanticRuntimePrepareFutureV1
            + Send
            + 'static,
    >,
}

impl SemanticRuntimeWorkV1 {
    pub fn new<Prepare, PrepareFuture>(
        target_generation: CodeGenerationId,
        total_units: u64,
        prepare: Prepare,
    ) -> Self
    where
        Prepare:
            FnOnce(Arc<SemanticRuntimeScheduleCancellationV1>) -> PrepareFuture + Send + 'static,
        PrepareFuture: Future<
                Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>,
            > + Send
            + 'static,
    {
        Self {
            target_generation,
            target_projection_key: None,
            total_units: total_units.max(1),
            prepare: Box::new(move |cancellation| Box::pin(prepare(cancellation))),
        }
    }

    pub fn new_with_projection<Prepare, PrepareFuture>(
        target_generation: CodeGenerationId,
        target_projection_key: ProjectionKeyV1,
        total_units: u64,
        prepare: Prepare,
    ) -> Self
    where
        Prepare:
            FnOnce(Arc<SemanticRuntimeScheduleCancellationV1>) -> PrepareFuture + Send + 'static,
        PrepareFuture: Future<
                Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>,
            > + Send
            + 'static,
    {
        let mut work = Self::new(target_generation, total_units, prepare);
        work.target_projection_key = Some(target_projection_key);
        work
    }

    pub fn total_units(&self) -> u64 {
        self.total_units
    }
}

struct SemanticRuntimeSchedulingStateV1 {
    sequence: u64,
    status: SemanticRuntimeScheduleStatusV1,
    current: Option<SemanticGenerationPointerV1>,
    cancellation: Option<Arc<SemanticRuntimeScheduleCancellationV1>>,
    committing: bool,
    accepting_work: bool,
}

impl Default for SemanticRuntimeSchedulingStateV1 {
    fn default() -> Self {
        Self {
            sequence: 0,
            status: SemanticRuntimeScheduleStatusV1::Unavailable,
            current: None,
            cancellation: None,
            committing: false,
            accepting_work: true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SemanticRuntimeShutdownReceiptV1 {
    pub joined_workers: usize,
    pub aborted_workers: usize,
    pub remaining_workers: usize,
}

impl SemanticRuntimeShutdownReceiptV1 {
    pub fn is_clean(&self) -> bool {
        self.remaining_workers == 0
    }
}

#[derive(Clone)]
pub struct SemanticRuntimeSchedulingHandleV1 {
    state: Arc<Mutex<SemanticRuntimeSchedulingStateV1>>,
    workers: Arc<Mutex<BTreeMap<u64, JoinHandle<()>>>>,
}

impl Default for SemanticRuntimeSchedulingHandleV1 {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(SemanticRuntimeSchedulingStateV1::default())),
            workers: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

impl SemanticRuntimeSchedulingHandleV1 {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start one bounded preparation task without waiting for artifact I/O,
    /// model loading, projection, or publication.
    ///
    /// A task already inside its serialized atomic commit is not displaced;
    /// callers receive `false` and may schedule the newer generation again.
    pub fn schedule(&self, work: SemanticRuntimeWorkV1) -> bool {
        let mut workers = self.workers.lock().unwrap_or_else(PoisonError::into_inner);
        workers.retain(|_, worker| !worker.is_finished());
        let (sequence, cancellation) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if !state.accepting_work || state.committing {
                return false;
            }
            if let Some(cancellation) = state.cancellation.take() {
                cancellation.cancel();
            }
            state.sequence = state.sequence.wrapping_add(1);
            let sequence = state.sequence;
            let cancellation =
                Arc::new(SemanticRuntimeScheduleCancellationV1::new(work.total_units));
            state.status = SemanticRuntimeScheduleStatusV1::Indexing {
                target_generation: work.target_generation.clone(),
                target_projection_key: work.target_projection_key.clone(),
                completed_units: 0,
                total_units: work.total_units,
                prior_generation: state
                    .current
                    .as_ref()
                    .map(|pointer| pointer.generation.clone()),
            };
            state.cancellation = Some(Arc::clone(&cancellation));
            (sequence, cancellation)
        };

        let handle = self.clone();
        let watcher = self.clone();
        let worker = tokio::spawn(async move {
            let worker = tokio::spawn(async move {
                let prepared = (work.prepare)(Arc::clone(&cancellation)).await;
                let prepared = match prepared {
                    Ok(prepared) if !cancellation.cancelled() => prepared,
                    Ok(_) => {
                        handle
                            .finish_failure(sequence, SemanticRuntimeScheduleFailureV1::Cancelled);
                        return;
                    }
                    Err(reason) => {
                        handle.finish_failure(sequence, reason);
                        return;
                    }
                };

                {
                    let mut state = handle.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if state.sequence != sequence || cancellation.cancelled() || state.committing {
                        return;
                    }
                    state.committing = true;
                }

                let committed = prepared.commit().await;
                let published = {
                    let mut state = handle.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if state.sequence != sequence
                        || cancellation.cancelled()
                        || !state.accepting_work
                    {
                        state.committing = false;
                        return;
                    }
                    let published = match committed {
                        Ok((pointer, install, published)) => {
                            if let Some(install) = install
                                && let Err(reason) = install(&pointer)
                            {
                                state.committing = false;
                                state.cancellation = None;
                                state.status = SemanticRuntimeScheduleStatusV1::Failed {
                                    reason,
                                    prior_generation: state
                                        .current
                                        .as_ref()
                                        .map(|pointer| pointer.generation.clone()),
                                };
                                return;
                            }
                            state.current = Some(pointer.clone());
                            state.status = SemanticRuntimeScheduleStatusV1::Current {
                                generation: pointer.generation.clone(),
                            };
                            published.map(|published| (published, pointer))
                        }
                        Err(reason) => {
                            state.status = SemanticRuntimeScheduleStatusV1::Failed {
                                reason,
                                prior_generation: state
                                    .current
                                    .as_ref()
                                    .map(|pointer| pointer.generation.clone()),
                            };
                            None
                        }
                    };
                    state.committing = false;
                    state.cancellation = None;
                    published
                };
                if let Some((published, pointer)) = published {
                    published(pointer).await;
                }
            });
            let mut abort_on_drop = AbortWorkerOnDrop::new(worker.abort_handle());
            let outcome = worker.await;
            abort_on_drop.disarm();
            if outcome.is_err() {
                watcher.finish_worker_terminated(sequence);
            }
        });
        workers.insert(sequence, worker);
        true
    }

    /// Permanently fence new projection work and signal the active worker.
    pub fn begin_shutdown(&self) -> bool {
        let _workers = self.workers.lock().unwrap_or_else(PoisonError::into_inner);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.accepting_work {
            return false;
        }
        state.accepting_work = false;
        if let Some(cancellation) = state.cancellation.as_ref() {
            cancellation.cancel();
        }
        true
    }

    /// Join every projection worker against the caller's one global deadline.
    ///
    /// Workers remaining at the deadline are aborted and then awaited before
    /// this method returns, so a clean receipt proves no worker escaped.
    pub async fn cancel_and_join_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> SemanticRuntimeShutdownReceiptV1 {
        self.begin_shutdown();
        let mut workers = {
            let mut registry = self.workers.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut *registry)
                .into_iter()
                .collect::<Vec<_>>()
        };
        let mut joined_workers = 0;
        let mut aborted_workers = 0;

        let now = tokio::time::Instant::now();
        let abort_at = deadline
            .checked_sub(Duration::from_millis(50))
            .unwrap_or(now)
            .max(now);
        while !workers.is_empty() && tokio::time::Instant::now() < abort_at {
            match tokio::time::timeout_at(abort_at, join_next_worker(&mut workers)).await {
                Ok(true) => joined_workers += 1,
                Ok(false) | Err(_) => break,
            }
        }

        if !workers.is_empty() {
            aborted_workers = workers.len();
            for (_, worker) in &workers {
                worker.abort();
            }
            self.normalize_shutdown_terminal(SemanticRuntimeScheduleFailureV1::Cancelled);
        }
        while !workers.is_empty() && tokio::time::Instant::now() < deadline {
            match tokio::time::timeout_at(deadline, join_next_worker(&mut workers)).await {
                Ok(true) => {}
                Ok(false) | Err(_) => break,
            }
        }
        let mut registry = self.workers.lock().unwrap_or_else(PoisonError::into_inner);
        for (sequence, worker) in workers {
            registry.insert(sequence, worker);
        }
        let remaining_workers = registry.len();
        drop(registry);
        SemanticRuntimeShutdownReceiptV1 {
            joined_workers,
            aborted_workers,
            remaining_workers,
        }
    }

    pub fn status(&self) -> SemanticRuntimeScheduleStatusV1 {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut status = state.status.clone();
        if let (
            SemanticRuntimeScheduleStatusV1::Indexing {
                completed_units, ..
            },
            Some(cancellation),
        ) = (&mut status, state.cancellation.as_ref())
        {
            *completed_units = cancellation.completed_units();
        }
        status
    }

    pub fn current(&self) -> Option<SemanticGenerationPointerV1> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .current
            .clone()
    }

    pub fn restore_current(&self, pointer: SemanticGenerationPointerV1) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(cancellation) = state.cancellation.take() {
            cancellation.cancel();
        }
        state.sequence = state.sequence.wrapping_add(1);
        state.committing = false;
        state.current = Some(pointer.clone());
        state.status = SemanticRuntimeScheduleStatusV1::Current {
            generation: pointer.generation,
        };
    }

    pub fn clear_current_if(&self, expected: &SemanticGenerationPointerV1) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.committing || state.current.as_ref() != Some(expected) {
            return false;
        }
        if let Some(cancellation) = state.cancellation.take() {
            cancellation.cancel();
        }
        state.sequence = state.sequence.wrapping_add(1);
        state.current = None;
        state.status = SemanticRuntimeScheduleStatusV1::Unavailable;
        true
    }

    pub fn cancel(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.committing {
            return false;
        }
        let Some(cancellation) = state.cancellation.take() else {
            return false;
        };
        cancellation.cancel();
        state.sequence = state.sequence.wrapping_add(1);
        state.status = SemanticRuntimeScheduleStatusV1::Failed {
            reason: SemanticRuntimeScheduleFailureV1::Cancelled,
            prior_generation: state
                .current
                .as_ref()
                .map(|pointer| pointer.generation.clone()),
        };
        true
    }

    fn finish_failure(&self, sequence: u64, reason: SemanticRuntimeScheduleFailureV1) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.sequence == sequence && !state.committing {
            state.cancellation = None;
            state.status = SemanticRuntimeScheduleStatusV1::Failed {
                reason,
                prior_generation: state
                    .current
                    .as_ref()
                    .map(|pointer| pointer.generation.clone()),
            };
        }
    }

    fn finish_worker_terminated(&self, sequence: u64) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.sequence == sequence {
            state.committing = false;
            state.cancellation = None;
            state.status = SemanticRuntimeScheduleStatusV1::Failed {
                reason: SemanticRuntimeScheduleFailureV1::Runtime,
                prior_generation: state
                    .current
                    .as_ref()
                    .map(|pointer| pointer.generation.clone()),
            };
        }
    }

    fn normalize_shutdown_terminal(&self, reason: SemanticRuntimeScheduleFailureV1) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.committing = false;
        state.cancellation = None;
        state.status = SemanticRuntimeScheduleStatusV1::Failed {
            reason,
            prior_generation: state
                .current
                .as_ref()
                .map(|pointer| pointer.generation.clone()),
        };
    }
}

struct AbortWorkerOnDrop {
    handle: Option<tokio::task::AbortHandle>,
}

impl AbortWorkerOnDrop {
    fn new(handle: tokio::task::AbortHandle) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn disarm(&mut self) {
        self.handle = None;
    }
}

impl Drop for AbortWorkerOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

async fn join_next_worker(workers: &mut Vec<(u64, JoinHandle<()>)>) -> bool {
    std::future::poll_fn(|context| {
        for index in (0..workers.len()).rev() {
            if Pin::new(&mut workers[index].1).poll(context).is_ready() {
                let _ = workers.swap_remove(index);
                return std::task::Poll::Ready(true);
            }
        }
        if workers.is_empty() {
            std::task::Poll::Ready(false)
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}
