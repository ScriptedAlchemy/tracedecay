use std::path::Path;

use tracedecay_domain::BranchGraphPublicationEpochV1;

use crate::errors::{Result, TraceDecayError};

use super::super::TraceDecay;
use super::BRANCH_QUERY_GRAPH_SOURCE_KEY;

fn branch_graph_source_for_root(
    project_root: &Path,
    project_id: String,
    branch: &str,
    source_oid: &str,
    publication_epoch: BranchGraphPublicationEpochV1,
) -> Result<crate::branch_meta::BranchGraphSourceV1> {
    let worktree_root = project_root
        .canonicalize()
        .map_err(|error| TraceDecayError::Config {
            message: format!("branch graph worktree root is unavailable: {error}"),
        })?;
    let repository_id = crate::daemon::code_index_scheduler::identity::repository_id_for(
        &worktree_root,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("branch graph repository identity is unavailable: {error}"),
    })?;
    let worktree_id = crate::daemon::code_index_scheduler::identity::worktree_id_for(
        &worktree_root,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("branch graph worktree identity is unavailable: {error}"),
    })?;
    Ok(crate::branch_meta::BranchGraphSourceV1 {
        publication_epoch,
        project_id,
        repository_id: repository_id.to_string(),
        worktree_id: worktree_id.to_string(),
        worktree_root: worktree_root.to_string_lossy().into_owned(),
        reference: format!("refs/heads/{branch}"),
        source_oid: source_oid.to_owned(),
    })
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct BranchGraphUpdatingV1 {
    publication_epoch: BranchGraphPublicationEpochV1,
    state: BranchGraphUpdatingStateV1,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum BranchGraphUpdatingStateV1 {
    Updating,
}

pub(super) struct BranchGraphPublicationV1 {
    publication_epoch: Option<BranchGraphPublicationEpochV1>,
}

pub(in crate::tracedecay) struct BranchGraphMutationV1 {
    sync_lease: super::super::locking::ActiveSyncLease,
    live_branch: crate::branch::BranchMemo,
    publication: BranchGraphPublicationV1,
}

fn next_branch_graph_publication_epoch(
    external_epoch: Option<BranchGraphPublicationEpochV1>,
    graph_marker: Option<&str>,
) -> Result<BranchGraphPublicationEpochV1> {
    let graph_epoch = graph_marker
        .and_then(|encoded| {
            serde_json::from_str::<crate::branch_meta::BranchGraphSourceV1>(encoded)
                .map(|source| source.publication_epoch)
                .or_else(|_| {
                    serde_json::from_str::<BranchGraphUpdatingV1>(encoded)
                        .map(|updating| updating.publication_epoch)
                })
                .ok()
        })
        .map_or(0, BranchGraphPublicationEpochV1::get);
    let next = external_epoch
        .map_or(0, BranchGraphPublicationEpochV1::get)
        .max(graph_epoch)
        .checked_add(1)
        .ok_or_else(|| TraceDecayError::Config {
            message: "branch graph publication epoch is exhausted".to_owned(),
        })?;
    BranchGraphPublicationEpochV1::new(next).map_err(|error| TraceDecayError::Config {
        message: format!("branch graph publication epoch is invalid: {error}"),
    })
}

impl TraceDecay {
    #[cfg(test)]
    pub(crate) async fn published_branch_graph_source(
        &self,
    ) -> Option<crate::branch_meta::BranchGraphSourceV1> {
        let encoded = self
            .db
            .get_metadata(BRANCH_QUERY_GRAPH_SOURCE_KEY)
            .await
            .ok()
            .flatten()?;
        serde_json::from_str(&encoded).ok()
    }

    /// Starts a graph publication epoch before any rows become visible.
    ///
    /// The sync lock serializes the read/advance/write sequence across
    /// processes. Persisting the new epoch in the updating marker also makes
    /// crash retries advance instead of reusing an abandoned identity.
    pub(super) async fn invalidate_branch_query_publication(
        &self,
        live_branch: &crate::branch::BranchMemo,
    ) -> Result<BranchGraphPublicationV1> {
        let Some(branch) = live_branch.resolve_for(&self.project_root) else {
            return Ok(BranchGraphPublicationV1 {
                publication_epoch: None,
            });
        };
        let Some(meta) = crate::branch_meta::load_branch_meta(&self.store_layout.data_root) else {
            return Ok(BranchGraphPublicationV1 {
                publication_epoch: None,
            });
        };
        let Some(entry) = meta.branches.get(&branch) else {
            return Ok(BranchGraphPublicationV1 {
                publication_epoch: None,
            });
        };
        let external_epoch = entry
            .graph_source
            .as_ref()
            .map(|source| source.publication_epoch);
        let graph_marker = self.db.get_metadata(BRANCH_QUERY_GRAPH_SOURCE_KEY).await?;
        let publication_epoch =
            next_branch_graph_publication_epoch(external_epoch, graph_marker.as_deref())?;
        let updating = serde_json::to_string(&BranchGraphUpdatingV1 {
            publication_epoch,
            state: BranchGraphUpdatingStateV1::Updating,
        })
        .map_err(|error| TraceDecayError::Config {
            message: format!("branch graph updating marker encoding failed: {error}"),
        })?;
        self.db
            .set_metadata(BRANCH_QUERY_GRAPH_SOURCE_KEY, &updating)
            .await?;
        Ok(BranchGraphPublicationV1 {
            publication_epoch: Some(publication_epoch),
        })
    }

    pub(super) async fn publish_branch_meta_synced(
        &self,
        live_branch: &crate::branch::BranchMemo,
        source_oid: Option<&str>,
        publication: &BranchGraphPublicationV1,
    ) -> Result<()> {
        let Some(publication_epoch) = publication.publication_epoch else {
            return Ok(());
        };
        let Some(branch) = live_branch.resolve_for(&self.project_root) else {
            return Ok(());
        };
        if !crate::branch_meta::load_branch_meta(&self.store_layout.data_root)
            .is_some_and(|meta| meta.is_tracked(&branch))
        {
            return Ok(());
        }
        let Some(source_oid) = source_oid else {
            crate::branch_meta::update_synced_timestamp(&self.store_layout.data_root, &branch);
            return Ok(());
        };
        self.db
            .get_metadata(BRANCH_QUERY_GRAPH_SOURCE_KEY)
            .await?
            .and_then(|encoded| serde_json::from_str::<BranchGraphUpdatingV1>(&encoded).ok())
            .filter(|updating| updating.publication_epoch == publication_epoch)
            .ok_or_else(|| TraceDecayError::Config {
                message: "branch graph publication epoch changed before commit".to_owned(),
            })?;
        let project_id = self
            .store_layout
            .identity
            .project_id
            .clone()
            .ok_or_else(|| TraceDecayError::Config {
                message: "branch graph publication requires a project identity".to_owned(),
            })?;
        let source = branch_graph_source_for_root(
            &self.project_root,
            project_id,
            &branch,
            source_oid,
            publication_epoch,
        )?;
        let encoded = serde_json::to_string(&source).map_err(|error| TraceDecayError::Config {
            message: format!("branch graph source encoding failed: {error}"),
        })?;
        // External ownership moves first while the graph-local marker remains
        // in-progress. Only the final DB write makes the new epoch queryable.
        crate::branch_meta::publish_graph_source(&self.store_layout.data_root, &branch, source)
            .map_err(|error| TraceDecayError::Config {
                message: format!("branch graph source publication failed: {error}"),
            })?;
        self.db
            .set_metadata(BRANCH_QUERY_GRAPH_SOURCE_KEY, &encoded)
            .await?;
        Ok(())
    }

    pub(in crate::tracedecay) async fn begin_branch_graph_mutation(
        &self,
        operation: &str,
    ) -> Result<BranchGraphMutationV1> {
        let live_branch = self.branch_memo();
        self.ensure_branch_writable_with(operation, &live_branch)?;
        let sync_lease = self.begin_active_sync()?;
        let publication = self
            .invalidate_branch_query_publication(&live_branch)
            .await?;
        Ok(BranchGraphMutationV1 {
            sync_lease,
            live_branch,
            publication,
        })
    }

    pub(in crate::tracedecay) async fn commit_branch_graph_mutation(
        &self,
        mutation: BranchGraphMutationV1,
    ) -> Result<()> {
        let source_oid = self.stamp_last_synced_commit().await;
        self.publish_branch_meta_synced(
            &mutation.live_branch,
            source_oid.as_deref(),
            &mutation.publication,
        )
        .await?;
        mutation.sync_lease.commit()
    }

    pub(in crate::tracedecay) async fn resolve_all_within_branch_graph_mutation(
        &self,
        _mutation: &BranchGraphMutationV1,
    ) -> Result<()> {
        self.resolve_all_unresolved_refs().await
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::{
        BranchGraphPublicationEpochV1, BranchGraphUpdatingStateV1, BranchGraphUpdatingV1,
        branch_graph_source_for_root, next_branch_graph_publication_epoch,
    };

    fn git(root: &std::path::Path, args: &[&str]) {
        let status = Command::new(crate::git::git_program())
            .current_dir(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "TraceDecay Test")
            .env("GIT_AUTHOR_EMAIL", "tracedecay@example.invalid")
            .env("GIT_COMMITTER_NAME", "TraceDecay Test")
            .env("GIT_COMMITTER_EMAIL", "tracedecay@example.invalid")
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn linked_worktree_sync_source_uses_the_graph_owning_worktree() {
        let fixture = tempfile::tempdir().expect("fixture");
        let primary = fixture.path().join("primary");
        let linked = fixture.path().join("linked");
        std::fs::create_dir(&primary).expect("primary");
        git(&primary, &["init", "-b", "main"]);
        std::fs::write(primary.join("tracked.txt"), "main\n").expect("file");
        git(&primary, &["add", "tracked.txt"]);
        git(&primary, &["commit", "-m", "initial"]);
        git(&primary, &["branch", "feature"]);
        git(
            &primary,
            &[
                "worktree",
                "add",
                linked.to_str().expect("linked"),
                "feature",
            ],
        );
        let oid = crate::git::git_capture(&linked, &["rev-parse", "HEAD"]).expect("oid");

        let source = branch_graph_source_for_root(
            &linked,
            "project.fixture".to_owned(),
            "feature",
            &oid,
            BranchGraphPublicationEpochV1::new(1).expect("epoch"),
        )
        .expect("source");
        let primary_worktree =
            crate::daemon::code_index_scheduler::identity::worktree_id_for(&primary)
                .expect("primary worktree");

        assert_eq!(
            std::path::Path::new(&source.worktree_root),
            linked.canonicalize().expect("canonical linked")
        );
        assert_ne!(source.worktree_id, primary_worktree.as_str());
        assert_eq!(source.reference, "refs/heads/feature");
        assert_eq!(source.source_oid, oid);
    }

    #[test]
    fn publication_epoch_advances_across_completion_and_crash_retry() {
        let source = crate::branch_meta::BranchGraphSourceV1 {
            publication_epoch: BranchGraphPublicationEpochV1::new(7).expect("epoch"),
            project_id: "project.fixture".to_owned(),
            repository_id: "repository.fixture".to_owned(),
            worktree_id: "worktree.fixture".to_owned(),
            worktree_root: "/fixture".to_owned(),
            reference: "refs/heads/main".to_owned(),
            source_oid: "a".repeat(40),
        };
        let published = serde_json::to_string(&source).expect("published source");
        assert_eq!(
            next_branch_graph_publication_epoch(
                Some(BranchGraphPublicationEpochV1::new(7).expect("external epoch")),
                Some(&published),
            )
            .expect("same owner and oid")
            .get(),
            8,
        );

        let updating = serde_json::to_string(&BranchGraphUpdatingV1 {
            publication_epoch: BranchGraphPublicationEpochV1::new(8).expect("epoch"),
            state: BranchGraphUpdatingStateV1::Updating,
        })
        .expect("updating marker");
        assert_eq!(
            next_branch_graph_publication_epoch(
                Some(BranchGraphPublicationEpochV1::new(7).expect("external epoch")),
                Some(&updating),
            )
            .expect("crash retry")
            .get(),
            9,
        );
    }
}
