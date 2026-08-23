//! The composition-root authority one bounded transcript ingest pass runs on.
//!
//! Catch-up needs four things that only the composition root can build: the
//! registered shard identity it is bound to, an admission facade over that
//! authority, the transcript/git/workflow store adapters that write through it,
//! and the registry read that enumerates registered project roots.
//!
//! Every one of those is produced from `RegisteredGlobalDb`, which sits above
//! this crate. Rather than depend on it, ingest states what it needs as this
//! port and lets the root hand the composed pieces back.

use std::future::Future;
use std::path::PathBuf;

use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
use tracedecay_store::StoreShardIdV1;

use crate::admission::HostAdmission;
use crate::repository_provenance::RepositoryProvenanceAdmissionContext;
use crate::runtime::git_correlation::GitCorrelationSessionStore;
use crate::runtime::store_port::TranscriptIngestStore;
use crate::runtime::workflow_index::WorkflowIngestSink;

pub enum IngestAdmissionBinding<'a> {
    /// Project-scoped admission, optionally carrying the repository
    /// provenance read from the project's identity marker.
    Project {
        brain_id: &'a BrainId,
        profile_id: &'a UserProfileId,
        project_id: &'a ProjectId,
        repository_provenance: Option<RepositoryProvenanceAdmissionContext>,
    },
    /// Profile-scoped admission for user-global transcript sources.
    Profile {
        brain_id: &'a BrainId,
        profile_id: &'a UserProfileId,
    },
}

pub trait SessionIngestAuthority: Sync {
    type GitStore<'store>: GitCorrelationSessionStore
    where
        Self: 'store;

    type WorkflowSink<'store>: WorkflowIngestSink
    where
        Self: 'store;

    type TranscriptStore<'store>: TranscriptIngestStore
    where
        Self: 'store;

    /// Shard identity the authority is registered for. Catch-up refuses to
    /// run when it does not match the brain/profile/scope it was asked for.
    fn shard_id(&self) -> &StoreShardIdV1;

    fn admission<'a>(&'a self, binding: IngestAdmissionBinding<'a>) -> Box<dyn HostAdmission + 'a>;

    fn git_correlation_store(&self) -> Self::GitStore<'_>;

    fn workflow_sink(&self) -> Self::WorkflowSink<'_>;

    fn transcript_store(&self) -> Self::TranscriptStore<'_>;

    /// Enumerates every registered project root known to this authority's
    /// registry. `None` means the registry could not be read, which is
    /// distinct from "no projects are registered".
    fn registered_project_roots(&self) -> impl Future<Output = Option<Vec<PathBuf>>> + Send;
}
