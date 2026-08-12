//! Finalization of shipped proposal history after the main typed terminal.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_agent_hosts::automation::automatic_facts::ShippedFactProposalDisposition;
use tracedecay_application::{DirectorySyncPolicy, retained_surfaces::AutomationTaskV1};

use crate::errors::{Result, TraceDecayError};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct RetirementBinding {
    pub(super) source_digest: String,
    pub(super) archive_name: String,
}

pub(super) struct RetirementPlan {
    pub(super) binding: RetirementBinding,
    source_path: PathBuf,
    source_bytes: Vec<u8>,
}

pub(super) fn classify(
    disposition: ShippedFactProposalDisposition,
) -> Result<RetirementClassification> {
    match disposition {
        ShippedFactProposalDisposition::Absent => Ok(RetirementClassification::Absent),
        ShippedFactProposalDisposition::ResetRequired {
            source_path,
            source_digest,
            reason,
        } => {
            validate_digest(&source_digest)?;
            Ok(RetirementClassification::ResetRequired {
                source_digest,
                detail: format!(
                    "{reason} at '{}'; final-V2 will not approve, import, archive, or delete unresolved shipped proposal state",
                    source_path.display()
                ),
            })
        }
        ShippedFactProposalDisposition::TerminalHistory {
            source_path,
            source_digest,
            source_bytes,
        } => {
            require_digest(&source_bytes, &source_digest)?;
            let archive_name = format!(
                "fact_proposals.{}.json",
                source_digest.trim_start_matches("sha256:")
            );
            Ok(RetirementClassification::Terminal(RetirementPlan {
                binding: RetirementBinding {
                    source_digest,
                    archive_name,
                },
                source_path,
                source_bytes,
            }))
        }
    }
}

pub(super) async fn classify_for_task(
    task: AutomationTaskV1,
    dashboard_root: &Path,
) -> Result<RetirementClassification> {
    if task != AutomationTaskV1::SessionReflector {
        return Ok(RetirementClassification::Absent);
    }
    classify(
        tracedecay_agent_hosts::automation::automatic_facts::inspect_shipped_fact_proposals(
            dashboard_root,
        )
        .await?,
    )
}

pub(super) enum RetirementClassification {
    Absent,
    ResetRequired {
        source_digest: String,
        detail: String,
    },
    Terminal(RetirementPlan),
}

pub(super) fn verify_plan_matches_binding(
    plan: &RetirementPlan,
    binding: &RetirementBinding,
) -> Result<()> {
    if plan.binding == *binding {
        Ok(())
    } else {
        Err(contract_error(
            "live shipped proposal history conflicts with its admitted retirement",
        ))
    }
}

/// Completes only after the caller has durably persisted the main typed
/// zero-effect `AutomationRun` terminal that contains this binding in
/// its admitted input digest.
pub(super) fn finalize_after_terminal(
    dashboard_root: &Path,
    binding: &RetirementBinding,
    live_plan: Option<&RetirementPlan>,
) -> Result<()> {
    validate_binding(binding)?;
    let source_path = dashboard_root.join("fact_proposals.json");
    let archive_path = archive_path(dashboard_root, binding)?;
    let bytes = match live_plan {
        Some(plan) => {
            verify_plan_matches_binding(plan, binding)?;
            if plan.source_path != source_path {
                return Err(contract_error(
                    "shipped proposal retirement source escaped dashboard authority",
                ));
            }
            plan.source_bytes.clone()
        }
        None => match std::fs::read(&source_path) {
            Ok(bytes) => {
                require_digest(&bytes, &binding.source_digest)?;
                bytes
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let bytes = std::fs::read(&archive_path).map_err(|archive_error| {
                    contract_error(format!(
                        "retired proposal history is absent from both live and archive paths: {archive_error}"
                    ))
                })?;
                require_digest(&bytes, &binding.source_digest)?;
                tracedecay_application::sync_parent_directory(
                    &source_path,
                    DirectorySyncPolicy::Strict,
                )
                .map_err(contract_error)?;
                return Ok(());
            }
            Err(error) => {
                return Err(contract_error(format!(
                    "shipped proposal source read failed: {error}"
                )));
            }
        },
    };
    publish_archive(&archive_path, &bytes)?;
    remove_exact_source(&source_path, &bytes)
}

fn publish_archive(path: &Path, bytes: &[u8]) -> Result<()> {
    match std::fs::read(path) {
        Ok(existing) if existing == bytes => Ok(()),
        Ok(_) => Err(contract_error(
            "shipped proposal retirement archive conflicts with admitted bytes",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracedecay_application::atomic_write(
                path,
                "shipped-proposal-retirement-archive",
                bytes,
                DirectorySyncPolicy::Strict,
            )
            .map_err(contract_error)
        }
        Err(error) => Err(contract_error(format!(
            "shipped proposal archive read failed: {error}"
        ))),
    }
}

fn remove_exact_source(path: &Path, expected: &[u8]) -> Result<()> {
    match std::fs::read(path) {
        Ok(current) if current == expected => {}
        Ok(_) => {
            return Err(contract_error(
                "shipped proposal source changed after typed retirement terminal",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracedecay_application::sync_parent_directory(path, DirectorySyncPolicy::Strict)
                .map_err(contract_error)?;
            return Ok(());
        }
        Err(error) => {
            return Err(contract_error(format!(
                "shipped proposal source read failed: {error}"
            )));
        }
    }
    std::fs::remove_file(path).map_err(|error| {
        contract_error(format!(
            "shipped proposal source removal failed after typed retirement: {error}"
        ))
    })?;
    tracedecay_application::sync_parent_directory(path, DirectorySyncPolicy::Strict)
        .map_err(contract_error)
}

fn archive_path(dashboard_root: &Path, binding: &RetirementBinding) -> Result<PathBuf> {
    if binding.archive_name.contains('/') || binding.archive_name.contains('\\') {
        return Err(contract_error("retirement archive name is not a basename"));
    }
    Ok(dashboard_root
        .join("fact_proposals.archive")
        .join(&binding.archive_name))
}

fn validate_binding(binding: &RetirementBinding) -> Result<()> {
    validate_digest(&binding.source_digest)?;
    let raw = binding.source_digest.trim_start_matches("sha256:");
    if binding.archive_name != format!("fact_proposals.{raw}.json") {
        return Err(contract_error(
            "retirement archive basename is not digest-derived",
        ));
    }
    Ok(())
}

fn validate_digest(digest: &str) -> Result<()> {
    let raw = digest.trim_start_matches("sha256:");
    if digest.starts_with("sha256:")
        && raw.len() == 64
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(contract_error("retirement digest is not canonical SHA-256"))
    }
}

fn require_digest(bytes: &[u8], expected: &str) -> Result<()> {
    let actual = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
    if actual == expected {
        Ok(())
    } else {
        Err(contract_error(
            "shipped proposal bytes do not match admitted retirement digest",
        ))
    }
}

fn contract_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("shipped proposal retirement is invalid: {error}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn terminal_shipped_sidecar() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "proposals": [
                {
                    "schema_version": 1,
                    "proposal_id": "fact_0123456789abcdef",
                    "run_id": "run-shipped-sidecar",
                    "evidence_hash": "shipped-evidence-hash",
                    "state": "applied",
                    "proposal": {
                        "content": "Preserve shipped proposal provenance",
                        "source_span": {"message_id": "msg-shipped"}
                    },
                    "validation": {"status": "accepted"},
                    "applied_fact_id": 42,
                    "apply_outcome": {"state": "applied", "fact_id": 42},
                    "created_at": 1_700_000_000,
                    "updated_at": 1_700_000_001,
                    "duplicate_count": 2,
                    "last_duplicate_run_id": "run-shipped-duplicate",
                    "folded_contents": ["Earlier wording"]
                },
                {
                    "schema_version": 1,
                    "proposal_id": "fact_fedcba9876543210",
                    "run_id": "run-shipped-sidecar",
                    "state": "rejected",
                    "proposal": {"content": "Transient rejected item"},
                    "validation_reason": "not durable",
                    "reviewer": "validator",
                    "created_at": 1_700_000_002,
                    "updated_at": 1_700_000_003
                }
            ]
        })
    }

    fn plan(root: &Path, bytes: &[u8]) -> RetirementPlan {
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        RetirementPlan {
            binding: RetirementBinding {
                source_digest: digest.clone(),
                archive_name: format!(
                    "fact_proposals.{}.json",
                    digest.trim_start_matches("sha256:")
                ),
            },
            source_path: root.join("fact_proposals.json"),
            source_bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn typed_terminal_finalizer_archives_then_removes_exact_bytes() {
        let root = tempfile::tempdir().unwrap();
        let bytes = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), bytes);
        std::fs::write(&plan.source_path, bytes).unwrap();

        finalize_after_terminal(root.path(), &plan.binding, Some(&plan)).unwrap();

        assert!(!plan.source_path.exists());
        assert_eq!(
            std::fs::read(archive_path(root.path(), &plan.binding).unwrap()).unwrap(),
            bytes
        );
    }

    #[test]
    fn replay_recovers_when_source_was_removed_after_archive() {
        let root = tempfile::tempdir().unwrap();
        let bytes = br#"{"schema_version":1,"proposals":[]}"#;
        let plan = plan(root.path(), bytes);
        let archive = archive_path(root.path(), &plan.binding).unwrap();
        tracedecay_application::atomic_write(
            &archive,
            "test-retirement-archive",
            bytes,
            DirectorySyncPolicy::Strict,
        )
        .unwrap();

        finalize_after_terminal(root.path(), &plan.binding, None).unwrap();
    }

    #[test]
    fn changed_source_is_never_removed() {
        let root = tempfile::tempdir().unwrap();
        let original = br#"{"schema_version":1,"proposals":[]}"#;
        let changed = br#"{"schema_version":1,"proposals":[{}]}"#;
        let plan = plan(root.path(), original);
        std::fs::write(&plan.source_path, changed).unwrap();

        assert!(finalize_after_terminal(root.path(), &plan.binding, None).is_err());
        assert_eq!(std::fs::read(&plan.source_path).unwrap(), changed);
    }

    #[tokio::test]
    async fn memory_curator_leaves_terminal_sidecar_for_session_reflector_retirement() {
        let root = tempfile::tempdir().unwrap();
        let source_path = root.path().join("fact_proposals.json");
        let source_bytes = serde_json::to_vec_pretty(&terminal_shipped_sidecar()).unwrap();
        tokio::fs::write(&source_path, &source_bytes).await.unwrap();

        let curator = classify_for_task(
            tracedecay_application::retained_surfaces::AutomationTaskV1::MemoryCurator,
            root.path(),
        )
        .await
        .unwrap();
        assert!(matches!(curator, RetirementClassification::Absent));
        assert_eq!(tokio::fs::read(&source_path).await.unwrap(), source_bytes);

        let reflector = classify_for_task(
            tracedecay_application::retained_surfaces::AutomationTaskV1::SessionReflector,
            root.path(),
        )
        .await
        .unwrap();
        assert!(matches!(reflector, RetirementClassification::Terminal(_)));
        assert_eq!(tokio::fs::read(&source_path).await.unwrap(), source_bytes);
    }
}
