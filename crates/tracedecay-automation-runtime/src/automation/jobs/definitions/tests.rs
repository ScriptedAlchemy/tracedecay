use std::fs::{File, FileTimes};
use std::sync::Arc;

use serde_json::json;

use super::ScheduledJobDefinitions;
use crate::automation::config::AutomationSchedule;
use crate::automation::jobs::{job_is_schedulable, job_schedule_decision, jobs_path};

fn document(schedule: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema_version": 1,
        "jobs": [{
            "id": "summary", "name": "Summary", "prompt": "summarize",
            "enabled": true, "schedule": schedule,
            "delivery": {"mode": "file"}
        }]
    }))
    .unwrap()
}

#[tokio::test]
async fn unchanged_ticks_and_concurrent_readers_share_admitted_definitions() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(jobs_path(root.path()), document("hourly")).unwrap();
    let definitions = ScheduledJobDefinitions::new(root.path());
    let first = definitions.read().await.unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(
        first[0].schedule(),
        AutomationSchedule::Interval { every_secs: 3600 }
    );
    for now in [0, 1, 60, 3600] {
        let (left, right) = tokio::join!(definitions.read(), definitions.read());
        for observed in [left.unwrap(), right.unwrap()] {
            assert!(
                Arc::ptr_eq(&first, &observed),
                "unchanged reads must reuse the admitted document"
            );
            assert!(job_is_schedulable(&observed[0]));
            assert_eq!(job_schedule_decision(&observed[0], &[], now), None);
        }
    }
}

#[tokio::test]
async fn same_length_rewrite_with_preserved_timestamp_replaces_timing_not_accepted_work() {
    let root = tempfile::tempdir().unwrap();
    let path = jobs_path(root.path());
    let hourly = document("hourly");
    let weekly = document("weekly");
    assert_eq!(hourly.len(), weekly.len());
    std::fs::write(&path, &hourly).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let definitions = ScheduledJobDefinitions::new(root.path());
    let accepted = definitions.read().await.unwrap();
    std::fs::write(&path, &weekly).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    let changed = definitions.read().await.unwrap();
    assert!(!Arc::ptr_eq(&accepted, &changed));
    assert_eq!(
        changed[0].schedule(),
        AutomationSchedule::Interval { every_secs: 604800 }
    );
    assert_eq!(
        accepted[0].schedule(),
        AutomationSchedule::Interval { every_secs: 3600 }
    );
    assert!(Arc::ptr_eq(&changed, &definitions.read().await.unwrap()));
}

#[tokio::test]
async fn malformed_and_unreadable_sources_refuse_instead_of_serving_previous_jobs() {
    let root = tempfile::tempdir().unwrap();
    let path = jobs_path(root.path());
    let valid = document("hourly");
    std::fs::write(&path, &valid).unwrap();
    let definitions = ScheduledJobDefinitions::new(root.path());
    let first = definitions.read().await.unwrap();
    for invalid in [
        b"{ broken".to_vec(),
        document("after lunch"),
        document("interval"),
    ] {
        std::fs::write(&path, &invalid).unwrap();
        let failure = definitions.read().await.unwrap_err().to_string();
        assert_eq!(definitions.read().await.unwrap_err().to_string(), failure);
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        definitions.read().await.is_err(),
        "a directory is not absent configuration"
    );
    std::fs::remove_dir(&path).unwrap();
    assert!(definitions.read().await.unwrap().is_empty());
    std::fs::write(&path, &valid).unwrap();
    let restored = definitions.read().await.unwrap();
    assert_eq!(&*first, &*restored);
    assert!(!Arc::ptr_eq(&first, &restored));
}

#[tokio::test]
async fn retired_owner_and_other_profiles_do_not_reuse_an_admission() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    for root in [&left, &right] {
        std::fs::write(jobs_path(root.path()), document("hourly")).unwrap();
    }
    let owner = ScheduledJobDefinitions::new(left.path());
    let first = owner.read().await.unwrap();
    let other = ScheduledJobDefinitions::new(right.path())
        .read()
        .await
        .unwrap();
    assert_eq!(&*first, &*other);
    assert!(!Arc::ptr_eq(&first, &other));
    drop(owner);
    let successor = ScheduledJobDefinitions::new(left.path())
        .read()
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &successor));
    assert_eq!(&*first, &*successor);
}

#[tokio::test]
async fn disabled_or_manual_replacement_removes_scheduled_work() {
    let root = tempfile::tempdir().unwrap();
    let path = jobs_path(root.path());
    let definitions = ScheduledJobDefinitions::new(root.path());
    std::fs::write(&path, document("hourly")).unwrap();
    let first = definitions.read().await.unwrap();
    std::fs::write(&path, document("manual")).unwrap();
    assert!(definitions.read().await.unwrap().is_empty());
    let mut disabled: serde_json::Value = serde_json::from_slice(&document("after lunch")).unwrap();
    disabled["jobs"][0]["enabled"] = json!(false);
    std::fs::write(&path, serde_json::to_vec(&disabled).unwrap()).unwrap();
    assert!(definitions.read().await.unwrap().is_empty());
    assert_eq!(job_schedule_decision(&first[0], &[], 3600), None);
}
