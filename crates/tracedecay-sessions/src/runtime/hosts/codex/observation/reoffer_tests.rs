//! Empty Codex re-offers must not pay repository resolution costs or cache a
//! scope decision that can leak into the next nonempty admission page.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{ObservationScopeV1, ProjectId};
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_runtime_core::git_discovery::{
    GitRepositoryIdentityOutcome, discover_repository_identity_cli_first,
};
use tracedecay_runtime_core::path_safety::plain_git_args;

use super::{
    CodexJsonlAdmissionProgress, CodexObservationAdmission, codex_observation_source_v2,
    try_admit_codex_jsonl_observations,
};
use crate::admission::test_support::MemoryHostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::observation::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority;
use crate::runtime::shared::ProjectRootMatcherCache;
use crate::runtime::with_transcript_source_profile;

static IDENTITY_RESOLUTIONS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn recording_identity(path: &Path) -> GitRepositoryIdentityOutcome {
    IDENTITY_RESOLUTIONS
        .lock()
        .unwrap()
        .push(path.to_path_buf());
    discover_repository_identity_cli_first(path)
}

fn resolution_count(root: &Path) -> usize {
    IDENTITY_RESOLUTIONS
        .lock()
        .unwrap()
        .iter()
        .filter(|path| path.starts_with(root))
        .count()
}

fn message(session: &str, item: &str) -> Value {
    json!({
        "timestamp": "2026-09-04T12:00:01.004Z",
        "type": "event_msg",
        "payload": {
            "type": "item_completed",
            "thread_id": session,
            "turn_id": item,
            "item": {
                "type": "UserMessage",
                "id": item,
                "content": [{"type": "text", "text": item}]
            }
        }
    })
}

fn write_rollout(path: &Path, cwd: &Path, session: &str) -> u64 {
    let mut file = std::fs::File::create(path).unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "timestamp": "2026-09-04T12:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": cwd}
        })
    )
    .unwrap();
    writeln!(file, "{}", message(session, "initial-item")).unwrap();
    file.metadata().unwrap().len()
}

fn append_in_scope(path: &Path, cwd: &Path, session: &str, item: &str) -> u64 {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap();
    writeln!(
        file,
        "{}",
        json!({"type": "turn_context", "payload": {"cwd": cwd, "model": "scope-model"}})
    )
    .unwrap();
    writeln!(file, "{}", message(session, item)).unwrap();
    file.metadata().unwrap().len()
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    project: PathBuf,
    outside: PathBuf,
    profile: ProfileRoot,
    project_id: ProjectId,
    project_admission: MemoryHostAdmission,
    profile_admission: MemoryHostAdmission,
}

impl Fixture {
    fn new() -> Self {
        install_test_shared_jsonl_preparation_authority();
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let project = root.join("project");
        let outside = root.join("outside");
        for repository in [&project, &outside] {
            std::fs::create_dir_all(repository.join("src")).unwrap();
            let output = std::process::Command::new(
                tracedecay_runtime_core::git::try_git_program().unwrap(),
            )
            .args(plain_git_args(&["init", "--quiet", "--initial-branch=main"]))
            .current_dir(repository)
            .output()
            .unwrap();
            assert!(
                output.status.success(),
                "git init: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Self {
            profile: ProfileRoot::under_home(root.join("owner")),
            _temp: temp,
            root,
            project,
            outside,
            project_id: ProjectId::new("project.codex-idle-reoffer").unwrap(),
            project_admission: MemoryHostAdmission::default(),
            profile_admission: MemoryHostAdmission::default(),
        }
    }

    async fn admit(
        &self,
        path: &Path,
        cache: &ProjectRootMatcherCache,
        cancellation: &ObservationCancellation,
    ) -> [CodexJsonlAdmissionProgress; 2] {
        let project = try_admit_codex_jsonl_observations(
            path,
            CodexObservationAdmission::Project {
                root: &self.project,
                project_id: self.project_id.clone(),
            },
            &self.project_admission,
            None,
            None,
            cancellation,
            cache,
        )
        .await
        .unwrap();
        let profile = try_admit_codex_jsonl_observations(
            path,
            CodexObservationAdmission::Profile {
                session_id: None,
                registered_roots: std::slice::from_ref(&self.project),
            },
            &self.profile_admission,
            None,
            None,
            cancellation,
            cache,
        )
        .await
        .unwrap();
        [project, profile]
    }
}

fn assert_unchanged(progress: CodexJsonlAdmissionProgress, len: u64) {
    assert!(progress.resumed);
    assert!(!progress.source_deferred);
    assert_eq!(progress.covered_through, len);
    assert_eq!(progress.bytes_consumed, 0);
    assert_eq!(progress.frames_decoded, 0);
    assert_eq!(progress.frames_persisted, 0);
    assert_eq!(progress.prior_context_bytes, 0);
}

#[test]
fn unchanged_corpus_reoffers_skip_cold_scope_resolution() {
    let fixture = Fixture::new();
    let mut rollouts = Vec::new();
    for index in 0..16 {
        let repository = if index % 2 == 0 {
            &fixture.project
        } else {
            &fixture.outside
        };
        let cwd = repository.join(format!("cwd-{index}"));
        std::fs::create_dir_all(&cwd).unwrap();
        let path = fixture.root.join(format!("rollout-{index}.jsonl"));
        let len = write_rollout(&path, &cwd, &format!("reoffer-session-{index}"));
        rollouts.push((path, len));
    }
    tracedecay_runtime_core::native_runtime::block_on(with_transcript_source_profile(
        fixture.profile.clone(),
        async {
            let cancellation = ObservationCancellation::default();
            let initial = ProjectRootMatcherCache::with_identity_resolver(recording_identity);
            for (path, len) in &rollouts {
                for progress in fixture.admit(path, &initial, &cancellation).await {
                    assert!(!progress.source_deferred);
                    assert_eq!(progress.covered_through, *len);
                    assert_eq!(progress.bytes_consumed, *len);
                }
            }
            let stores = [&fixture.project_admission, &fixture.profile_admission];
            assert_eq!(stores.map(|store| store.observations().len()), [16, 16]);
            let captures = stores.map(MemoryHostAdmission::capture_call_counts);
            let retained = stores.map(|store| {
                store
                    .observations()
                    .iter()
                    .map(|record| {
                        (
                            record.observation().observation_id().clone(),
                            record.committed_cursor().clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            });
            let before = resolution_count(&fixture.root);
            assert!(before >= rollouts.len(), "the resolver probe must be live");
            drop(initial);

            // A cold process cache cannot hide redundant resolution on the
            // first empty re-offer. Later rounds also exercise the warm path.
            let cache = ProjectRootMatcherCache::with_identity_resolver(recording_identity);
            let started = Instant::now();
            let mut reoffers = 0;
            for _ in 0..3 {
                for (path, len) in &rollouts {
                    for progress in fixture.admit(path, &cache, &cancellation).await {
                        assert_unchanged(progress, *len);
                        reoffers += 1;
                    }
                }
            }
            let resolutions = resolution_count(&fixture.root) - before;
            tracing::info!(
                reoffers,
                git_identity_resolutions = resolutions,
                wall_seconds = started.elapsed().as_secs_f64(),
                "unchanged Codex corpus re-offer measurement"
            );
            assert_eq!(reoffers, 96);
            assert_eq!(resolutions, 0, "empty pages must not resolve Git scope");
            assert_eq!(stores.map(MemoryHostAdmission::capture_call_counts), captures);
            for (store, retained) in stores.into_iter().zip(retained) {
                let current = store
                    .observations()
                    .iter()
                    .map(|record| {
                        (
                            record.observation().observation_id().clone(),
                            record.committed_cursor().clone(),
                        )
                    })
                    .collect::<Vec<_>>();
                assert_eq!(current, retained, "accepted source identity must not change");
            }
        },
    ))
    .unwrap();
}

#[test]
fn append_after_empty_reoffer_rechecks_scope_and_source_identity() {
    let fixture = Fixture::new();
    let path = fixture.root.join("rollout-live.jsonl");
    let session = "reoffer-live-session";
    let len = write_rollout(&path, &fixture.project.join("src"), session);
    tracedecay_runtime_core::native_runtime::block_on(with_transcript_source_profile(
        fixture.profile.clone(),
        async {
            let cancellation = ObservationCancellation::default();
            let initial = ProjectRootMatcherCache::default();
            let admitted = fixture.admit(&path, &initial, &cancellation).await;
            assert_eq!(admitted[0].frames_persisted, 2);
            assert_eq!(admitted[1].frames_persisted, 0);
            drop(initial);

            let cache = ProjectRootMatcherCache::with_identity_resolver(recording_identity);
            for progress in fixture.admit(&path, &cache, &cancellation).await {
                assert_unchanged(progress, len);
            }
            assert_eq!(resolution_count(&fixture.root), 0);

            let new_len = append_in_scope(
                &path,
                &fixture.outside.join("src"),
                session,
                "outside-item",
            );
            let appended = fixture.admit(&path, &cache, &cancellation).await;
            assert_eq!(appended[0].frames_persisted, 0);
            assert!(appended[1].frames_persisted > 0);
            for progress in appended {
                assert!(!progress.source_deferred);
                assert_eq!(progress.covered_through, new_len);
                assert_eq!(progress.bytes_consumed, new_len - len);
            }
            assert!(resolution_count(&fixture.root) > 0);
            let expected_source = codex_observation_source_v2(session).unwrap();
            let profile_records = fixture.profile_admission.observations();
            assert!(!profile_records.is_empty());
            for record in &profile_records {
                assert_eq!(record.observation().source(), &expected_source);
                assert_eq!(record.observation().scope(), &ObservationScopeV1::Profile);
            }
            assert_eq!(fixture.project_admission.observations().len(), 2);

            let final_len = append_in_scope(
                &path,
                &fixture.project.join("src"),
                session,
                "returned-item",
            );
            let returned = fixture.admit(&path, &cache, &cancellation).await;
            assert!(returned[0].frames_persisted > 0);
            assert_eq!(returned[1].frames_persisted, 0);
            for progress in returned {
                assert!(!progress.source_deferred);
                assert_eq!(progress.covered_through, final_len);
                assert_eq!(progress.bytes_consumed, final_len - new_len);
            }
            assert_eq!(
                fixture.profile_admission.observations().len(),
                profile_records.len()
            );
            for progress in fixture.admit(&path, &cache, &cancellation).await {
                assert_unchanged(progress, final_len);
            }

            cancellation.cancel();
            let captures = fixture.project_admission.capture_call_counts();
            for progress in fixture.admit(&path, &cache, &cancellation).await {
                assert!(progress.source_deferred, "cancellation is not convergence");
                assert_eq!(progress.frames_persisted, 0);
            }
            assert_eq!(fixture.project_admission.capture_call_counts(), captures);
        },
    ))
    .unwrap();
}
