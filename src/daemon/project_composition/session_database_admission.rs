use std::future::Future;
use std::path::Path;
use std::time::Duration;

use crate::errors::Result;

use super::log_daemon_event;

pub(super) async fn join_independent_session_opens<Project, Profile, ProjectOpen, ProfileOpen>(
    project_open: ProjectOpen,
    profile_open: ProfileOpen,
) -> Result<(Project, Profile)>
where
    ProjectOpen: Future<Output = Result<Project>>,
    ProfileOpen: Future<Output = Result<Profile>>,
{
    tokio::try_join!(project_open, profile_open)
}

pub(super) fn log_session_database_admission(
    project: &Path,
    project_elapsed: Duration,
    profile_elapsed: Duration,
) {
    for (phase, elapsed) in [
        ("project_sessions_admitted", project_elapsed),
        ("profile_sessions_admitted", profile_elapsed),
    ] {
        log_daemon_event(
            "project_open_phase",
            &[
                ("project", project.display().to_string()),
                ("phase", phase.to_owned()),
                ("elapsed_ms", elapsed.as_millis().to_string()),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{Barrier, oneshot};
    use tracedecay_domain::{BrainId, LocatorDigest, UserProfileId};
    use tracedecay_store::{ProjectId, StoreIncarnationV1, StoreShardIdV1, VerifiedStoreLocatorV1};

    use super::join_independent_session_opens;
    use crate::errors::TraceDecayError;

    type ExactSessionIdentity = (StoreShardIdV1, VerifiedStoreLocatorV1);

    fn exact_session_identities() -> (ExactSessionIdentity, ExactSessionIdentity) {
        let brain = BrainId::new("brain.concurrent-session-opens").expect("brain id");
        let profile = UserProfileId::new("profile.concurrent-session-opens").expect("profile id");
        let project_shard = StoreShardIdV1::project_sessions(
            brain.clone(),
            profile.clone(),
            ProjectId::new("project.concurrent-session-opens").expect("project id"),
        );
        let profile_shard = StoreShardIdV1::profile_sessions(brain, profile);
        let incarnation = StoreIncarnationV1::new(41).expect("incarnation");
        let project_locator = VerifiedStoreLocatorV1::new(
            project_shard.clone(),
            incarnation,
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).expect("project locator"),
        );
        let profile_locator = VerifiedStoreLocatorV1::new(
            profile_shard.clone(),
            incarnation,
            LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).expect("profile locator"),
        );
        (
            (project_shard, project_locator),
            (profile_shard, profile_locator),
        )
    }

    #[tokio::test]
    async fn independent_session_opens_preserve_identity_in_either_completion_order() {
        for project_first in [true, false] {
            let (project_identity, profile_identity) = exact_session_identities();
            let expected_project = project_identity.clone();
            let expected_profile = profile_identity.clone();
            let entered = Arc::new(Barrier::new(3));
            let (release_project, project_released) = oneshot::channel();
            let (release_profile, profile_released) = oneshot::channel();
            let project_entered = Arc::clone(&entered);
            let profile_entered = Arc::clone(&entered);
            let joined = tokio::spawn(join_independent_session_opens(
                async move {
                    project_entered.wait().await;
                    project_released.await.expect("release project open");
                    Ok(project_identity)
                },
                async move {
                    profile_entered.wait().await;
                    profile_released.await.expect("release profile open");
                    Ok(profile_identity)
                },
            ));

            tokio::time::timeout(Duration::from_secs(2), entered.wait())
                .await
                .expect("both independent opens must start before either completes");
            if project_first {
                release_project.send(()).expect("finish project first");
                tokio::task::yield_now().await;
                assert!(!joined.is_finished());
                release_profile.send(()).expect("finish profile second");
            } else {
                release_profile.send(()).expect("finish profile first");
                tokio::task::yield_now().await;
                assert!(!joined.is_finished());
                release_project.send(()).expect("finish project second");
            }
            let (project, profile) = joined
                .await
                .expect("join session admission task")
                .expect("admit both session databases");
            assert_eq!(project, expected_project);
            assert_eq!(profile, expected_profile);
            assert_ne!(project.0.scope, profile.0.scope);
            assert_ne!(project.1.locator_digest, profile.1.locator_digest);
        }
    }

    struct DropReceipt(Option<oneshot::Sender<()>>);

    impl Drop for DropReceipt {
        fn drop(&mut self) {
            if let Some(receipt) = self.0.take() {
                let _ = receipt.send(());
            }
        }
    }

    #[tokio::test]
    async fn failed_session_open_cancels_its_peer_without_partial_admission() {
        let entered = Arc::new(Barrier::new(3));
        let (dropped, drop_receipt) = oneshot::channel();
        let project_entered = Arc::clone(&entered);
        let profile_entered = Arc::clone(&entered);
        let joined = tokio::spawn(join_independent_session_opens(
            async move {
                let _drop_receipt = DropReceipt(Some(dropped));
                project_entered.wait().await;
                std::future::pending::<crate::errors::Result<ExactSessionIdentity>>().await
            },
            async move {
                profile_entered.wait().await;
                Err::<ExactSessionIdentity, _>(TraceDecayError::Config {
                    message: "profile session admission failed".to_owned(),
                })
            },
        ));

        tokio::time::timeout(Duration::from_secs(2), entered.wait())
            .await
            .expect("both independent opens must start before either fails");
        let error = joined
            .await
            .expect("join failed admission task")
            .expect_err("one failed open cannot expose a partial pair");
        assert!(matches!(
            error,
            TraceDecayError::Config { message }
                if message == "profile session admission failed"
        ));
        tokio::time::timeout(Duration::from_secs(2), drop_receipt)
            .await
            .expect("peer open must be cancelled within the lifecycle tripwire")
            .expect("peer cancellation receipt");
    }
}
