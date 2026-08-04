use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use serde::Deserialize;
use tracedecay_capture::kimi as kimi_capture;
use tracedecay_domain::{
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceIdentityV1, ProviderId,
    RetentionClass, SessionId,
};
use tracedecay_runtime_core::privacy::{
    ObservationRecordParseErrorV1, parse_normalized_observation_record_v1,
};
use tracedecay_store::observation::ObservationCoverageReason;

use crate::admission::HostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::jsonl_observation_admission::{
    JsonlFrameAdmission, JsonlObservationAdmissionRequest, admit_jsonl_observations,
};
use crate::runtime::shared::TranscriptScopeMatcher;
use crate::runtime::snapshot_observation::{
    MAX_SNAPSHOT_METADATA_BYTES, read_snapshot_text_bounded,
};
use crate::runtime::source::{
    FileDiscoveryReport, TranscriptDiscoveryBounds, TranscriptIngestError, TranscriptIngestResult,
    bound_path_list,
};

const PROVIDER: &str = "kimi";
const MAX_SESSION_FILES: usize = 512;

pub struct KimiSource {
    share_dir: PathBuf,
    user_registered_roots: Option<Vec<PathBuf>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KimiCaptureOutcome {
    pub bytes_consumed: u64,
    pub deferred: bool,
}

#[derive(Deserialize)]
struct KimiMetadata {
    #[serde(default)]
    work_dirs: Vec<KimiWorkDir>,
}

#[derive(Deserialize)]
struct KimiWorkDir {
    path: PathBuf,
    #[serde(default = "local_kaos")]
    kaos: String,
}

fn local_kaos() -> String {
    "local".to_owned()
}

impl KimiSource {
    pub fn new() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::with_share_dir(&home.join(".kimi")))
    }

    pub fn with_share_dir(share_dir: &Path) -> Self {
        Self {
            share_dir: share_dir.to_path_buf(),
            user_registered_roots: None,
        }
    }

    #[must_use]
    pub fn for_user_scope(mut self, registered_roots: Vec<PathBuf>) -> Self {
        self.user_registered_roots = Some(registered_roots);
        self
    }

    pub fn discover(
        &self,
        project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
    ) -> TranscriptIngestResult<FileDiscoveryReport> {
        let Some(metadata) = self.metadata()? else {
            return Ok(bound_path_list(Vec::new(), bounds));
        };
        let matcher =
            TranscriptScopeMatcher::for_scope(project_root, self.user_registered_roots.as_deref());
        let limit = bounds.max_files.min(MAX_SESSION_FILES);
        let mut paths = Vec::with_capacity(limit.saturating_add(1));
        'work_dirs: for work_dir in metadata.work_dirs {
            if !matcher.accepts(Some(&work_dir.path)) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(self.sessions_dir(&work_dir)) else {
                continue;
            };
            for entry in entries.flatten() {
                if paths.len() > limit {
                    break 'work_dirs;
                }
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_symlink() {
                    continue;
                }
                let path = entry.path();
                if file_type.is_file()
                    && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                {
                    paths.push(path);
                } else if file_type.is_dir() {
                    let context = path.join("context.jsonl");
                    if context.is_file() {
                        paths.push(context);
                    }
                }
            }
        }
        paths.sort();
        Ok(bound_path_list(
            paths,
            TranscriptDiscoveryBounds {
                max_files: limit,
                ..bounds
            },
        ))
    }

    fn metadata(&self) -> TranscriptIngestResult<Option<KimiMetadata>> {
        let Some(text) = read_snapshot_text_bounded(
            PROVIDER,
            &self.share_dir.join("kimi.json"),
            MAX_SNAPSHOT_METADATA_BYTES,
        )?
        else {
            return Ok(None);
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|_| TranscriptIngestError::NonDurableRecord {
                provider: PROVIDER,
                offset: 0,
                end_offset: text.len() as u64,
                reason: "malformed Kimi metadata JSON",
            })
    }

    fn sessions_dir(&self, work_dir: &KimiWorkDir) -> PathBuf {
        let digest = Md5::digest(work_dir.path.to_string_lossy().as_bytes());
        let hash = format!("{digest:x}");
        let directory = if matches!(work_dir.kaos.as_str(), "" | "local") {
            hash
        } else {
            format!("{}_{hash}", work_dir.kaos)
        };
        self.share_dir.join("sessions").join(directory)
    }
}

pub async fn capture_kimi_observations(
    facade: &dyn HostAdmission,
    source: &KimiSource,
    project_root: &Path,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<KimiCaptureOutcome> {
    let discovery = source.discover(
        project_root,
        TranscriptDiscoveryBounds::from_discovered_units(MAX_SESSION_FILES),
    )?;
    let mut outcome = KimiCaptureOutcome {
        deferred: discovery.is_truncated(),
        ..KimiCaptureOutcome::default()
    };
    let mut remaining = max_new_bytes.unwrap_or(u64::MAX);
    for path in discovery.paths {
        if cancellation.is_cancelled() || remaining == 0 {
            outcome.deferred = true;
            break;
        }
        let session_id = kimi_session_id(&path)?;
        let provider = ProviderId::new(PROVIDER).map_err(|_| invalid_frame())?;
        let session = SessionId::new(&session_id).map_err(|_| invalid_frame())?;
        let source_identity = ObservationSourceIdentityV1::for_provider(provider, session)
            .map_err(|_| invalid_frame())?;
        let retention = RetentionClass::new("transcript.kimi.v1").map_err(|_| invalid_frame())?;
        let request = JsonlObservationAdmissionRequest::new(
            PROVIDER,
            &path,
            facade,
            source_identity,
            scope.clone(),
            retention,
        )
        .with_max_new_bytes(max_new_bytes.map(|_| remaining))
        .with_cancellation(cancellation.clone());
        let progress = admit_jsonl_observations(
            request,
            |_| (),
            move |(), bytes, range, _| {
                let native_id = kimi_capture::native_record_id(&session_id, range)
                    .map_err(|_| invalid_frame())?;
                match parse_normalized_observation_record_v1(
                    bytes,
                    range,
                    ObservationOrderingDomainV1::FileBytes,
                    |native| {
                        kimi_capture::normalize_observation(
                            &native,
                            &session_id,
                            native_id.clone(),
                            range,
                        )
                    },
                ) {
                    Ok(parsed) => Ok(JsonlFrameAdmission::durable(parsed, native_id)),
                    Err(ObservationRecordParseErrorV1::Empty) => Ok(
                        JsonlFrameAdmission::non_durable(ObservationCoverageReason::BlankFrame),
                    ),
                    Err(
                        ObservationRecordParseErrorV1::TooLarge
                        | ObservationRecordParseErrorV1::CanonicalEnvelopeTooLarge,
                    ) => Ok(JsonlFrameAdmission::non_durable(
                        ObservationCoverageReason::OversizedFrame,
                    )),
                    Err(_) => Ok(JsonlFrameAdmission::non_durable(
                        ObservationCoverageReason::MalformedFrame,
                    )),
                }
            },
        )
        .await?;
        outcome.bytes_consumed = outcome
            .bytes_consumed
            .saturating_add(progress.bytes_consumed);
        outcome.deferred |= progress.source_deferred;
        remaining = remaining.saturating_sub(progress.bytes_consumed);
    }
    Ok(outcome)
}

fn kimi_session_id(path: &Path) -> TranscriptIngestResult<String> {
    let session_id = if path.file_name().and_then(|name| name.to_str()) == Some("context.jsonl") {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
    } else {
        path.file_stem().and_then(|name| name.to_str())
    };
    session_id
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: path.to_path_buf(),
        })
}

const fn invalid_frame() -> TranscriptIngestError {
    TranscriptIngestError::InvalidFrameState { provider: PROVIDER }
}

#[cfg(test)]
mod tests {
    use md5::{Digest, Md5};
    use serde_json::json;
    use tracedecay_domain::ObservationScopeV1;

    use crate::admission::test_support::MemoryHostAdmission;
    use crate::observation::ObservationCancellation;
    use crate::runtime::source::TranscriptDiscoveryBounds;

    use super::{KimiSource, capture_kimi_observations};

    fn fixture() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        KimiSource,
    ) {
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let share = temp.path().join("isolated-kimi");
        std::fs::create_dir_all(&share).unwrap();
        std::fs::write(
            share.join("kimi.json"),
            json!({"work_dirs": [{"path": project}]}).to_string(),
        )
        .unwrap();
        let hash = format!("{:x}", Md5::digest(project.to_string_lossy().as_bytes()));
        let transcript = share
            .join("sessions")
            .join(hash)
            .join("session-a")
            .join("context.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        (
            temp,
            project,
            transcript,
            KimiSource::with_share_dir(&share),
        )
    }

    #[tokio::test]
    async fn isolated_source_is_bounded_resumable_and_keeps_partial_prefix() {
        let (_temp, project, path, source) = fixture();
        let first = json!({"role": "user", "content": "first"}).to_string() + "\n";
        let second = json!({"role": "assistant", "content": "second"}).to_string() + "\n";
        std::fs::write(&path, format!("{first}{second}")).unwrap();
        let admission = MemoryHostAdmission::default();
        let cancellation = ObservationCancellation::default();

        let partial = capture_kimi_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            Some(first.len() as u64),
            &cancellation,
        )
        .await
        .unwrap();
        assert!(partial.deferred);
        assert_eq!(admission.observations().len(), 1);

        let resumed = capture_kimi_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &cancellation,
        )
        .await
        .unwrap();
        assert!(!resumed.deferred);
        assert_eq!(admission.observations().len(), 2);
    }

    #[tokio::test]
    async fn compaction_summary_flows_through_canonical_redaction_authority() {
        let (_temp, project, path, source) = fixture();
        std::fs::write(
            path,
            json!({
                "role": "assistant",
                "content": [{
                    "type": "text",
                    "text": "Previous context has been compacted. Here is the compaction output: summary",
                    "secret_key": "never-persist-kimi-secret"
                }]
            })
            .to_string()
                + "\n",
        )
        .unwrap();
        let admission = MemoryHostAdmission::default();

        capture_kimi_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap();

        let stored = admission
            .observations()
            .iter()
            .map(|observation| observation.observation().payload().to_string())
            .collect::<String>();
        assert!(stored.contains("compaction"));
        assert!(!stored.contains("never-persist-kimi-secret"));
    }

    #[test]
    fn discovery_is_scoped_and_reports_file_count_backpressure() {
        let (_temp, project, first, source) = fixture();
        std::fs::write(&first, "{}\n").unwrap();
        let second = first
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("session-b/context.jsonl");
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        std::fs::write(second, "{}\n").unwrap();

        let report = source
            .discover(
                &project,
                TranscriptDiscoveryBounds {
                    max_files: 1,
                    ..TranscriptDiscoveryBounds::default_walk()
                },
            )
            .unwrap();
        assert_eq!(report.paths.len(), 1);
        assert!(report.is_truncated());
        assert!(
            source
                .discover(
                    &project.join("unregistered"),
                    TranscriptDiscoveryBounds::default_walk()
                )
                .unwrap()
                .paths
                .is_empty()
        );
    }
}
