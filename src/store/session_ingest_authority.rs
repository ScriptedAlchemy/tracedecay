//! Root composition adapter for bounded session ingest.

use std::borrow::Borrow;
use std::future::Future;
use std::path::PathBuf;

use tracedecay_sessions::admission::HostAdmission;
use tracedecay_sessions::runtime::ingest::{IngestAdmissionBinding, SessionIngestAuthority};

use crate::global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use crate::store::{GlobalDbGitCorrelationStore, GlobalDbTranscriptStore, GlobalDbWorkflowStore};
use tracedecay_usecases::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};

/// Session-ingest authority over one registered database.
///
/// The holder `D` is generic so a caller that owns an
/// `RegisteredGlobalDbLeaseV1` can build a `'static` authority. That matters
/// beyond ergonomics: when the authority type carries a free lifetime, the
/// `SessionIngestAuthority` GATs plus the `admission` method's
/// `Box<dyn HostAdmission + 'a>` return push the auto-trait leak check into a
/// higher-ranked `for<'a> …: Send` obligation it cannot discharge, which
/// surfaces as "implementation of `Send` is not general enough" at every
/// `tokio::spawn`/boxed-future boundary downstream. A `'static` holder keeps
/// the obligation first-order. Borrowed holders remain supported for call
/// sites that never cross such a boundary.
pub(crate) struct GlobalDbSessionIngestAuthority<D> {
    db: D,
}

impl<D> GlobalDbSessionIngestAuthority<D>
where
    D: Borrow<RegisteredGlobalDb>,
{
    pub(crate) const fn new(db: D) -> Self {
        Self { db }
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

    fn registered_project_roots(&self) -> impl Future<Output = Option<Vec<PathBuf>>> + Send {
        async move {
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
    }
}
