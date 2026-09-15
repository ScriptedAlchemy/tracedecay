use std::collections::BTreeSet;
use std::path::PathBuf;

use super::{ProjectOpenTaskClaim, ProjectOpenTasks, ProjectRouteKey};

fn route(profile: &str, project: &str) -> ProjectRouteKey {
    ProjectRouteKey {
        profile_root: PathBuf::from(profile),
        global_db_path: PathBuf::from(profile).join("global.db"),
        project_path: PathBuf::from(project),
        scope_prefix: None,
    }
}

#[tokio::test]
async fn recovery_quiescence_fences_only_the_exact_project_open_identity() {
    let tasks = ProjectOpenTasks::default();
    let profile_a = PathBuf::from("/profiles/a");
    let root_a = PathBuf::from("/projects/a");
    let route_a = route("/profiles/a", "/projects/a");
    let route_b = route("/profiles/a", "/projects/b");

    assert!(matches!(
        tasks
            .start_cancellable(route_a.clone(), |cancelled| async move {
                cancelled.cancelled().await;
                Ok(())
            })
            .await,
        ProjectOpenTaskClaim::InFlight(_)
    ));
    let guard = tasks
        .quiesce_project_identity(&profile_a, "proj_shared", &BTreeSet::from([root_a.clone()]))
        .await
        .expect("exact project open quiescence");

    assert!(matches!(
        tasks.start(route_a.clone(), async { Ok(()) }).await,
        ProjectOpenTaskClaim::Failed(ref failure)
            if failure.message.contains("temporarily unavailable during remote recovery")
    ));
    assert!(matches!(
        tasks.start(route_b, async { Ok(()) }).await,
        ProjectOpenTaskClaim::InFlight(_)
    ));

    drop(guard);
    assert!(matches!(
        tasks.start(route_a, async { Ok(()) }).await,
        ProjectOpenTaskClaim::InFlight(_)
    ));
}
