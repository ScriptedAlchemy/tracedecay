use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_application::DirectorySyncPolicy;

use super::super::config_error;
use super::{
    AutomationRunArtifact, AutomationRunArtifactKind, RUN_ARTIFACTS_DIR,
    artifact_path_from_relative, artifact_relative_path, read_run_artifact_payload,
    validate_run_id_component,
};
use crate::errors::{Result, TraceDecayError};

const RUN_ARTIFACT_PUBLICATION_FILE: &str = ".tracedecay-publication.json";
const PRODUCT_ARTIFACT_KINDS: [AutomationRunArtifactKind; 6] = [
    AutomationRunArtifactKind::Traces,
    AutomationRunArtifactKind::Feedback,
    AutomationRunArtifactKind::GeneratedEvals,
    AutomationRunArtifactKind::ValidationGate,
    AutomationRunArtifactKind::OptimizerDiagnosis,
    AutomationRunArtifactKind::CodexHandoff,
];

fn run_artifact_publication_path(dashboard_root: &Path, run_id: &str) -> Result<PathBuf> {
    validate_run_id_component(run_id)?;
    Ok(dashboard_root
        .join(RUN_ARTIFACTS_DIR)
        .join(run_id)
        .join(RUN_ARTIFACT_PUBLICATION_FILE))
}

fn prepare_run_artifact_publication(
    run_id: &str,
    identity: &Value,
    artifacts: &[AutomationRunArtifact],
) -> Result<Vec<u8>> {
    validate_run_id_component(run_id)?;
    if identity.is_null() {
        return Err(config_error(
            "artifact publication identity must not be null",
        ));
    }
    serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1,
        "run_id": run_id,
        "identity": identity,
        "artifacts": artifacts,
    }))
    .map_err(TraceDecayError::from)
}

fn validate_product_artifact_descriptors(
    run_id: &str,
    artifacts: &[AutomationRunArtifact],
) -> Result<()> {
    validate_run_id_component(run_id)?;
    let mut seen = std::collections::BTreeSet::new();
    for artifact in artifacts {
        let kind = AutomationRunArtifactKind::parse(&artifact.kind)
            .ok_or_else(|| config_error(format!("unknown artifact kind '{}'", artifact.kind)))?;
        if !seen.insert(kind.as_str()) {
            return Err(config_error(format!(
                "artifact chain contains duplicate kind '{}'",
                artifact.kind
            )));
        }
        if artifact.path != artifact_relative_path(run_id, kind) {
            return Err(config_error(format!(
                "artifact '{}' does not use its canonical path",
                artifact.kind
            )));
        }
    }
    if artifacts.len() != PRODUCT_ARTIFACT_KINDS.len()
        || PRODUCT_ARTIFACT_KINDS
            .iter()
            .any(|kind| !seen.contains(kind.as_str()))
    {
        return Err(config_error(
            "artifact publication does not contain the complete product chain",
        ));
    }
    Ok(())
}

fn validate_product_artifact_chain(
    run_id: &str,
    artifacts: &[(AutomationRunArtifact, Vec<u8>)],
) -> Result<Vec<AutomationRunArtifact>> {
    let descriptors = artifacts
        .iter()
        .map(|(artifact, _)| artifact.clone())
        .collect::<Vec<_>>();
    validate_product_artifact_descriptors(run_id, &descriptors)?;
    for (artifact, bytes) in artifacts {
        if artifact.sha256 != super::super::artifact_refs::sha256_bytes(bytes) {
            return Err(config_error(format!(
                "artifact '{}' metadata hash does not match its bytes",
                artifact.kind
            )));
        }
    }
    Ok(descriptors)
}

pub async fn read_published_artifact_chain(
    dashboard_root: &Path,
    run_id: &str,
    expected_identity: Option<&Value>,
) -> Result<Option<Vec<AutomationRunArtifact>>> {
    let publication_path = run_artifact_publication_path(dashboard_root, run_id)?;
    crate::storage::reject_symlink_components(&publication_path, "automation artifact publication")
        .map_err(TraceDecayError::from)?;
    let bytes = match tokio::fs::read(&publication_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(config_error(format!(
                "failed to read artifact publication '{}': {error}",
                publication_path.display()
            )));
        }
    };
    let payload: Value = serde_json::from_slice(&bytes).map_err(TraceDecayError::from)?;
    if payload.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(config_error(
            "artifact publication has unsupported schema_version",
        ));
    }
    if payload.get("run_id").and_then(Value::as_str) != Some(run_id) {
        return Err(config_error(
            "artifact publication run_id does not match its path",
        ));
    }
    let identity = payload
        .get("identity")
        .filter(|identity| !identity.is_null())
        .ok_or_else(|| config_error("artifact publication is missing identity"))?;
    if let Some(expected_identity) = expected_identity
        && identity != expected_identity
    {
        return Err(config_error(
            "published artifact chain does not match the current run identity",
        ));
    }
    let artifacts = serde_json::from_value::<Vec<AutomationRunArtifact>>(
        payload
            .get("artifacts")
            .cloned()
            .ok_or_else(|| config_error("artifact publication is missing artifacts"))?,
    )
    .map_err(TraceDecayError::from)?;
    validate_product_artifact_descriptors(run_id, &artifacts)?;
    for artifact in &artifacts {
        read_run_artifact_payload(dashboard_root, run_id, artifact).await?;
    }
    let dashboard_root = dashboard_root.to_path_buf();
    let run_directory = publication_path
        .parent()
        .ok_or_else(|| config_error("artifact publication is missing its run directory"))?
        .to_path_buf();
    tokio::task::spawn_blocking(move || {
        sync_directory(&run_directory)?;
        let artifacts_root = run_directory
            .parent()
            .ok_or_else(|| config_error("artifact run is missing its root"))?;
        sync_directory(artifacts_root)?;
        sync_directory(&dashboard_root)
    })
    .await
    .map_err(|error| {
        config_error(format!("failed to join artifact durability fence: {error}"))
    })??;
    Ok(Some(artifacts))
}

pub(crate) async fn publish_run_artifact_chain(
    dashboard_root: &Path,
    run_id: &str,
    artifacts: Vec<(AutomationRunArtifact, Vec<u8>)>,
    identity: &Value,
) -> Result<()> {
    let artifact_descriptors = validate_product_artifact_chain(run_id, &artifacts)?;
    let publication = prepare_run_artifact_publication(run_id, identity, &artifact_descriptors)?;
    let dashboard_root = dashboard_root.to_path_buf();
    let run_id = run_id.to_string();
    tokio::task::spawn_blocking(move || {
        publish_run_artifact_chain_blocking(&dashboard_root, &run_id, &artifacts, &publication)
    })
    .await
    .map_err(|error| config_error(format!("failed to join artifact publication: {error}")))?
}

fn publish_run_artifact_chain_blocking(
    dashboard_root: &Path,
    run_id: &str,
    artifacts: &[(AutomationRunArtifact, Vec<u8>)],
    publication: &[u8],
) -> Result<()> {
    publish_run_artifact_chain_blocking_with_fault(
        dashboard_root,
        run_id,
        artifacts,
        publication,
        &mut |_| Ok(()),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactPublishBoundary {
    BeforeArtifact(usize),
    AfterArtifactSync(usize),
    BeforeStageSync,
    BeforeRename,
    AfterRename,
}

fn publish_run_artifact_chain_blocking_with_fault(
    dashboard_root: &Path,
    run_id: &str,
    artifacts: &[(AutomationRunArtifact, Vec<u8>)],
    publication: &[u8],
    fault: &mut impl FnMut(ArtifactPublishBoundary) -> Result<()>,
) -> Result<()> {
    validate_product_artifact_chain(run_id, artifacts)?;
    let artifacts_root = dashboard_root.join(RUN_ARTIFACTS_DIR);
    std::fs::create_dir_all(dashboard_root).map_err(|error| {
        config_error(format!(
            "failed to create dashboard root '{}': {error}",
            dashboard_root.display()
        ))
    })?;
    crate::storage::reject_symlink_components(&artifacts_root, "automation artifact")
        .map_err(TraceDecayError::from)?;
    std::fs::create_dir_all(&artifacts_root).map_err(|error| {
        config_error(format!(
            "failed to create artifact root '{}': {error}",
            artifacts_root.display()
        ))
    })?;
    sync_directory(dashboard_root)?;
    let final_directory = artifacts_root.join(run_id);
    crate::storage::reject_symlink_components(&final_directory, "automation artifact run")
        .map_err(TraceDecayError::from)?;
    let chain_matches = |directory: &Path| -> Result<bool> {
        for (artifact, expected) in artifacts {
            let path = artifact_path_from_relative(dashboard_root, run_id, &artifact.path)?;
            let filename = path
                .file_name()
                .ok_or_else(|| config_error("artifact path is missing a filename"))?;
            match std::fs::read(directory.join(filename)) {
                Ok(actual) if actual.as_slice() == expected.as_slice() => {}
                Ok(_) => return Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => {
                    return Err(config_error(format!(
                        "failed to verify artifact chain '{}': {error}",
                        directory.display()
                    )));
                }
            }
        }
        match std::fs::read(directory.join(RUN_ARTIFACT_PUBLICATION_FILE)) {
            Ok(actual) => Ok(actual.as_slice() == publication),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(config_error(format!(
                "failed to verify artifact publication '{}': {error}",
                directory.display()
            ))),
        }
    };
    if final_directory.exists() {
        return if chain_matches(&final_directory)? {
            sync_directory(&final_directory)?;
            sync_directory(&artifacts_root)?;
            sync_directory(dashboard_root)?;
            Ok(())
        } else {
            Err(config_error(format!(
                "artifact chain '{}' already exists with different content",
                final_directory.display()
            )))
        };
    }

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| config_error(format!("failed to generate artifact stage nonce: {error}")))?
        .as_nanos();
    let stage_directory =
        artifacts_root.join(format!(".stage-{run_id}-{}-{nonce}", std::process::id()));
    let mut publish = || -> Result<()> {
        std::fs::create_dir(&stage_directory).map_err(|error| {
            config_error(format!(
                "failed to create artifact stage '{}': {error}",
                stage_directory.display()
            ))
        })?;
        for (index, (artifact, bytes)) in artifacts.iter().enumerate() {
            fault(ArtifactPublishBoundary::BeforeArtifact(index))?;
            let destination = artifact_path_from_relative(dashboard_root, run_id, &artifact.path)?;
            let filename = destination
                .file_name()
                .ok_or_else(|| config_error("artifact path is missing a filename"))?;
            let staged = stage_directory.join(filename);
            write_staged_artifact(&staged, bytes)?;
            fault(ArtifactPublishBoundary::AfterArtifactSync(index))?;
        }
        let publication_index = artifacts.len();
        fault(ArtifactPublishBoundary::BeforeArtifact(publication_index))?;
        write_staged_artifact(
            &stage_directory.join(RUN_ARTIFACT_PUBLICATION_FILE),
            publication,
        )?;
        fault(ArtifactPublishBoundary::AfterArtifactSync(
            publication_index,
        ))?;
        fault(ArtifactPublishBoundary::BeforeStageSync)?;
        sync_directory(&stage_directory)?;
        fault(ArtifactPublishBoundary::BeforeRename)?;
        std::fs::rename(&stage_directory, &final_directory).map_err(|error| {
            config_error(format!(
                "failed to publish artifact chain '{}': {error}",
                final_directory.display()
            ))
        })?;
        fault(ArtifactPublishBoundary::AfterRename)?;
        sync_directory(&artifacts_root)
    };
    if let Err(error) = publish() {
        if let Err(cleanup_error) = std::fs::remove_dir_all(&stage_directory) {
            return Err(config_error(format!(
                "{error}; failed to clean artifact stage '{}': {cleanup_error}",
                stage_directory.display()
            )));
        }
        return Err(error);
    }
    Ok(())
}

fn write_staged_artifact(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let mut file = std::fs::File::create(path).map_err(|error| {
        config_error(format!(
            "failed to create staged artifact '{}': {error}",
            path.display()
        ))
    })?;
    file.write_all(bytes).map_err(|error| {
        config_error(format!(
            "failed to write staged artifact '{}': {error}",
            path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        config_error(format!(
            "failed to sync staged artifact '{}': {error}",
            path.display()
        ))
    })?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    tracedecay_application::sync_directory(path, DirectorySyncPolicy::Strict).map_err(|error| {
        config_error(format!(
            "failed to sync artifact directory '{}': {error}",
            path.display()
        ))
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::super::{prepare_run_artifact, run_ledger_path};
    use super::*;

    fn test_artifact_chain(run_id: &str) -> Vec<(AutomationRunArtifact, Vec<u8>)> {
        [
            AutomationRunArtifactKind::Traces,
            AutomationRunArtifactKind::Feedback,
            AutomationRunArtifactKind::GeneratedEvals,
            AutomationRunArtifactKind::ValidationGate,
            AutomationRunArtifactKind::OptimizerDiagnosis,
            AutomationRunArtifactKind::CodexHandoff,
        ]
        .into_iter()
        .map(|kind| {
            prepare_run_artifact(
                run_id,
                kind,
                &serde_json::json!({"kind": kind.as_str()}),
                None,
                "1",
            )
            .unwrap()
        })
        .collect()
    }

    fn test_artifact_publication(
        run_id: &str,
        artifacts: &[(AutomationRunArtifact, Vec<u8>)],
    ) -> Vec<u8> {
        prepare_run_artifact_publication(
            run_id,
            &serde_json::json!({"identity": "test"}),
            &artifacts
                .iter()
                .map(|(artifact, _)| artifact.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn publication_returns_only_product_artifacts() {
        let temp = tempfile::TempDir::new().unwrap();
        let run_id = "six-product-artifacts";
        let identity = serde_json::json!({"identity": "test"});
        let artifacts = test_artifact_chain(run_id);

        publish_run_artifact_chain(temp.path(), run_id, artifacts, &identity)
            .await
            .unwrap();

        let published = read_published_artifact_chain(temp.path(), run_id, Some(&identity))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(published.len(), 6);
        assert_eq!(
            published
                .iter()
                .map(|artifact| artifact.kind.as_str())
                .collect::<Vec<_>>(),
            [
                "traces",
                "feedback",
                "generated_evals",
                "validation_gate",
                "optimizer_diagnosis",
                "codex_handoff",
            ]
        );
        assert!(
            run_artifact_publication_path(temp.path(), run_id)
                .unwrap()
                .is_file()
        );
    }

    #[tokio::test]
    async fn incomplete_chain_is_rejected_before_filesystem_mutation() {
        let temp = tempfile::TempDir::new().unwrap();
        let run_id = "incomplete-chain";
        let identity = serde_json::json!({"identity": "test"});
        let mut artifacts = test_artifact_chain(run_id);
        artifacts.pop();

        let error = publish_run_artifact_chain(temp.path(), run_id, artifacts, &identity)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("complete product chain"));
        assert!(!temp.path().join(RUN_ARTIFACTS_DIR).exists());
    }

    #[tokio::test]
    async fn publication_requires_schema_version_one() {
        let temp = tempfile::TempDir::new().unwrap();
        let run_id = "wrong-schema";
        let identity = serde_json::json!({"identity": "test"});
        publish_run_artifact_chain(temp.path(), run_id, test_artifact_chain(run_id), &identity)
            .await
            .unwrap();
        let path = run_artifact_publication_path(temp.path(), run_id).unwrap();
        let mut payload: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        payload["schema_version"] = serde_json::json!(2);
        std::fs::write(&path, serde_json::to_vec(&payload).unwrap()).unwrap();

        let error = read_published_artifact_chain(temp.path(), run_id, None)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("schema_version"));
    }

    #[tokio::test]
    async fn publication_requires_identity_without_a_comparison_target() {
        let temp = tempfile::TempDir::new().unwrap();
        let run_id = "missing-identity";
        let identity = serde_json::json!({"identity": "test"});
        publish_run_artifact_chain(temp.path(), run_id, test_artifact_chain(run_id), &identity)
            .await
            .unwrap();
        let path = run_artifact_publication_path(temp.path(), run_id).unwrap();
        let mut payload: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        payload.as_object_mut().unwrap().remove("identity");
        std::fs::write(&path, serde_json::to_vec(&payload).unwrap()).unwrap();

        let error = read_published_artifact_chain(temp.path(), run_id, None)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("missing identity"));
    }

    #[test]
    fn artifact_publication_faults_never_expose_a_partial_final_chain() {
        let artifact_count = test_artifact_chain("boundaries").len();
        let mut boundaries = Vec::with_capacity((artifact_count + 1) * 2 + 2);
        for index in 0..=artifact_count {
            boundaries.push(ArtifactPublishBoundary::BeforeArtifact(index));
            boundaries.push(ArtifactPublishBoundary::AfterArtifactSync(index));
        }
        boundaries.extend([
            ArtifactPublishBoundary::BeforeStageSync,
            ArtifactPublishBoundary::BeforeRename,
        ]);
        for (index, boundary) in boundaries.into_iter().enumerate() {
            let temp = tempfile::TempDir::new().unwrap();
            let run_id = format!("fault-{index}");
            let artifacts = test_artifact_chain(&run_id);
            let publication = test_artifact_publication(&run_id, &artifacts);
            let error = publish_run_artifact_chain_blocking_with_fault(
                temp.path(),
                &run_id,
                &artifacts,
                &publication,
                &mut |current| {
                    if current == boundary {
                        Err(config_error("injected artifact publication fault"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("injected"));
            assert!(!temp.path().join(RUN_ARTIFACTS_DIR).join(&run_id).exists());
            assert!(!run_ledger_path(temp.path()).exists());
        }
    }

    #[test]
    fn post_rename_fault_leaves_a_complete_idempotent_chain_without_a_receipt() {
        let temp = tempfile::TempDir::new().unwrap();
        let run_id = "post-rename-fault";
        let artifacts = test_artifact_chain(run_id);
        let publication = test_artifact_publication(run_id, &artifacts);

        publish_run_artifact_chain_blocking_with_fault(
            temp.path(),
            run_id,
            &artifacts,
            &publication,
            &mut |boundary| {
                if boundary == ArtifactPublishBoundary::AfterRename {
                    Err(config_error("injected post-rename fault"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

        let final_directory = temp.path().join(RUN_ARTIFACTS_DIR).join(run_id);
        assert!(final_directory.is_dir());
        assert_eq!(
            std::fs::read_dir(&final_directory).unwrap().count(),
            artifacts.len() + 1
        );
        assert!(!run_ledger_path(temp.path()).exists());
        publish_run_artifact_chain_blocking(temp.path(), run_id, &artifacts, &publication).unwrap();
    }

    #[test]
    fn artifact_publication_rejects_non_canonical_metadata_paths() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut artifacts = test_artifact_chain("mismatched-path");
        artifacts[0].0.path = "automation_artifacts/mismatched-path/nested/traces.json".to_string();
        let publication = test_artifact_publication("mismatched-path", &artifacts);

        let error = publish_run_artifact_chain_blocking(
            temp.path(),
            "mismatched-path",
            &artifacts,
            &publication,
        )
        .unwrap_err();

        assert!(error.to_string().contains("canonical path"));
        assert!(
            !temp
                .path()
                .join(RUN_ARTIFACTS_DIR)
                .join("mismatched-path")
                .exists()
        );
    }

    #[cfg(unix)]
    #[test]
    fn artifact_publication_rejects_symlinked_run_directories() {
        let temp = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let artifacts_root = temp.path().join(RUN_ARTIFACTS_DIR);
        std::fs::create_dir_all(&artifacts_root).unwrap();
        std::os::unix::fs::symlink(outside.path(), artifacts_root.join("symlink-run")).unwrap();
        let artifacts = test_artifact_chain("symlink-run");
        let publication = test_artifact_publication("symlink-run", &artifacts);

        let error = publish_run_artifact_chain_blocking(
            temp.path(),
            "symlink-run",
            &artifacts,
            &publication,
        )
        .unwrap_err();

        assert!(error.to_string().contains("must not contain symlinks"));
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
