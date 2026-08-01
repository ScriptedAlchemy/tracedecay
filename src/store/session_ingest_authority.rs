//! Root composition adapter for bounded session ingest.

use std::future::Future;
use std::path::PathBuf;

use tracedecay_sessions::admission::HostAdmission;
use tracedecay_sessions::runtime::ingest::{IngestAdmissionBinding, SessionIngestAuthority};

use crate::application::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use crate::global_db::RegisteredGlobalDb;
use crate::store::{GlobalDbGitCorrelationStore, GlobalDbTranscriptStore, GlobalDbWorkflowStore};

/// Borrowed session-ingest authority over one registered database.
pub(crate) struct GlobalDbSessionIngestAuthority<'a> {
    db: &'a RegisteredGlobalDb,
}

impl<'a> GlobalDbSessionIngestAuthority<'a> {
    pub(crate) const fn new(db: &'a RegisteredGlobalDb) -> Self {
        Self { db }
    }
}

impl SessionIngestAuthority for GlobalDbSessionIngestAuthority<'_> {
    type GitStore<'store>
        = GlobalDbGitCorrelationStore<'store>
    where
        Self: 'store;

    type WorkflowSink<'store>
        = GlobalDbWorkflowStore<'store>
    where
        Self: 'store;

    type TranscriptStore<'store>
        = GlobalDbTranscriptStore<'store>
    where
        Self: 'store;

    fn shard_id(&self) -> &tracedecay_store::StoreShardIdV1 {
        &self.db.binding().shard_id
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
                    self.db,
                );
                match repository_provenance {
                    Some(provenance) => authorities.with_repository_provenance(provenance),
                    None => authorities,
                }
            }
            IngestAdmissionBinding::Profile {
                brain_id,
                profile_id,
            } => {
                HostAdmissionAuthorities::for_profile(brain_id.clone(), profile_id.clone(), self.db)
            }
        };
        Box::new(HostAdmissionFacade::new(authorities))
    }

    fn git_correlation_store(&self) -> Self::GitStore<'_> {
        GlobalDbGitCorrelationStore::new(self.db)
    }

    fn workflow_sink(&self) -> Self::WorkflowSink<'_> {
        GlobalDbWorkflowStore::new(self.db)
    }

    fn transcript_store(&self) -> Self::TranscriptStore<'_> {
        GlobalDbTranscriptStore::new(self.db)
    }

    fn registered_project_roots(&self) -> impl Future<Output = Option<Vec<PathBuf>>> + Send {
        async move {
            let mut roots = self.db.try_list_project_paths().await.ok()?;
            roots.extend(self.db.try_list_code_project_paths(usize::MAX).await.ok()?);
            roots.extend(self.db.try_list_project_alias_paths().await.ok()?);
            Some(roots)
        }
    }
}
