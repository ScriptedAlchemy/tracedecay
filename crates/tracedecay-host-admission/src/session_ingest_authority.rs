//! Registered-database composition adapter for bounded session ingest.

use std::borrow::Borrow;
use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_sessions::admission::HostAdmission;
use tracedecay_sessions::host_ports::session_review::SessionReviewPort;
use tracedecay_sessions::runtime::ingest::{IngestAdmissionBinding, SessionIngestAuthority};

use tracedecay_global_db::{
    GlobalDbGitCorrelationStore, GlobalDbWorkflowStore, RegisteredGlobalDb,
};
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_session_memory::transcript::GlobalDbTranscriptStore;

use crate::{HostAdmissionAuthorities, HostAdmissionFacade};

/// Session-ingest authority over one registered database.
///
/// The holder `D` is generic so a caller that owns an
/// [`tracedecay_global_db::RegisteredGlobalDbLeaseV1`] can build a `'static`
/// authority. That matters
/// beyond ergonomics: when the authority type carries a free lifetime, the
/// `SessionIngestAuthority` GATs plus the `admission` method's
/// `Box<dyn HostAdmission + 'a>` return push the auto-trait leak check into a
/// higher-ranked `for<'a> …: Send` obligation it cannot discharge, which
/// surfaces as "implementation of `Send` is not general enough" at every
/// `tokio::spawn`/boxed-future boundary downstream. A `'static` holder keeps
/// the obligation first-order. Borrowed holders remain supported for call
/// sites that never cross such a boundary.
pub struct GlobalDbSessionIngestAuthority<D> {
    db: D,
    /// The process background CPU authority every admission this authority
    /// issues prepares captures under. Ingest composition injects the one
    /// authority the daemon worker plan installed; read-only callers that only
    /// resolve registered roots leave it unset.
    background_cpu: Option<Arc<ProcessBackgroundCpuV1>>,
    /// The root-owned post-ingest review scheduler. Only user-global catch-up
    /// consults it, and refuses to run without it; project and read-only
    /// callers leave it unset.
    session_review: Option<SessionReviewPort>,
}

impl<D> GlobalDbSessionIngestAuthority<D>
where
    D: Borrow<RegisteredGlobalDb>,
{
    pub const fn new(db: D) -> Self {
        Self {
            db,
            background_cpu: None,
            session_review: None,
        }
    }

    /// Mounts the process background CPU authority observation capture is
    /// admitted through.
    #[must_use]
    pub fn with_background_cpu(mut self, background_cpu: Arc<ProcessBackgroundCpuV1>) -> Self {
        self.background_cpu = Some(background_cpu);
        self
    }

    /// Mounts the session review scheduler a user-global catch-up pass
    /// hands its freshly ingested sessions to.
    #[must_use]
    pub const fn with_session_review(mut self, session_review: SessionReviewPort) -> Self {
        self.session_review = Some(session_review);
        self
    }

    fn db(&self) -> &RegisteredGlobalDb {
        self.db.borrow()
    }
}

impl<D> SessionIngestAuthority for GlobalDbSessionIngestAuthority<D>
where
    D: Borrow<RegisteredGlobalDb> + Clone + Send + Sync,
{
    // Each borrowed store carries this authority's own holder rather than a
    // `&'store RegisteredGlobalDb`. With an owned (`Arc`) holder the projected
    // types stay lifetime-free, so their trait impls apply for any lifetime and
    // downstream `Send` proofs never go higher-ranked.
    type GitStore<'store>
        = GlobalDbGitCorrelationStore<D>
    where
        Self: 'store;

    type WorkflowSink<'store>
        = GlobalDbWorkflowStore<D>
    where
        Self: 'store;

    type TranscriptStore<'store>
        = GlobalDbTranscriptStore<D>
    where
        Self: 'store;

    fn shard_id(&self) -> &tracedecay_store::StoreShardIdV1 {
        &self.db().binding().shard_id
    }

    fn admission<'a>(&'a self, binding: IngestAdmissionBinding<'a>) -> Box<dyn HostAdmission + 'a> {
        let authorities = match binding {
            IngestAdmissionBinding::Project {
                brain_id,
                profile_id,
                project_id,
                repository_provenance,
            } => {
                let authorities = HostAdmissionAuthorities::for_project(
                    brain_id.clone(),
                    profile_id.clone(),
                    project_id.clone(),
                    self.db(),
                );
                match repository_provenance {
                    Some(provenance) => authorities.with_repository_provenance(provenance),
                    None => authorities,
                }
            }
            IngestAdmissionBinding::Profile {
                brain_id,
                profile_id,
            } => HostAdmissionAuthorities::for_profile(
                brain_id.clone(),
                profile_id.clone(),
                self.db(),
            ),
        };
        let authorities = match &self.background_cpu {
            Some(background_cpu) => authorities.with_background_cpu(Arc::clone(background_cpu)),
            None => authorities,
        };
        Box::new(HostAdmissionFacade::new(authorities))
    }

    fn git_correlation_store(&self) -> Self::GitStore<'_> {
        GlobalDbGitCorrelationStore::new(self.db.clone())
    }

    fn workflow_sink(&self) -> Self::WorkflowSink<'_> {
        GlobalDbWorkflowStore::new(self.db.clone())
    }

    fn transcript_store(&self) -> Self::TranscriptStore<'_> {
        GlobalDbTranscriptStore::new(self.db.clone())
    }

    #[hotpath::measure(label = "usecases.session_ingest.project_roots", future = true)]
    async fn registered_project_roots(&self) -> Option<Vec<PathBuf>> {
        let mut roots = self.db().try_list_project_paths().await.ok()?;
        roots.extend(
            self.db()
                .try_list_code_project_paths(usize::MAX)
                .await
                .ok()?,
        );
        roots.extend(self.db().try_list_project_alias_paths().await.ok()?);
        Some(roots)
    }

    fn session_review(&self) -> Option<SessionReviewPort> {
        self.session_review
    }
}
