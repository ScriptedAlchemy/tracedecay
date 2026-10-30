use std::sync::Arc;

use tracedecay_automation_runtime::automation::config::AutomationSchedule;
use tracedecay_automation_runtime::automation::jobs::{AutomationJob, JobDelivery, save_jobs};

use super::{TraceDecay, TraceDecayOpenOptions};

#[asupersync::test]
async fn project_reads_share_admission_until_the_job_document_changes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    let profile = temp.path().join("profile");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    let graph = TraceDecay::init_with_options_for_test(
        &root,
        TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(profile.join("global.db")),
        },
    )
    .await
    .unwrap();
    let mut job = AutomationJob {
        id: "summary".to_owned(),
        name: "Summary".to_owned(),
        prompt: "summarize".to_owned(),
        enabled: true,
        schedule: Some("hourly".to_owned()),
        interval_secs: None,
        cooldown_secs: None,
        skill_ids: Vec::new(),
        pre_run_command: None,
        delivery: JobDelivery::default(),
        created_at: 1,
        updated_at: 1,
        extra: std::collections::BTreeMap::default(),
    };
    let dashboard_root = &graph.store_layout().dashboard_root;
    save_jobs(dashboard_root, &[job.clone()]).await.unwrap();
    let first = graph.scheduled_jobs().await.unwrap();
    assert_eq!(first.len(), 1);
    assert!(Arc::ptr_eq(&first, &graph.scheduled_jobs().await.unwrap()));
    job.schedule = Some("weekly".to_owned());
    save_jobs(dashboard_root, &[job]).await.unwrap();
    let changed = graph.scheduled_jobs().await.unwrap();
    assert!(!Arc::ptr_eq(&first, &changed));
    assert_eq!(
        first[0].schedule(),
        AutomationSchedule::Interval { every_secs: 3600 }
    );
    assert_eq!(
        changed[0].schedule(),
        AutomationSchedule::Interval {
            every_secs: 604_800
        }
    );
    assert!(Arc::ptr_eq(
        &changed,
        &graph.scheduled_jobs().await.unwrap()
    ));
}
