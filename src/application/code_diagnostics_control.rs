//! Canonical project-scoped code-diagnostics settings and refresh operations.
//!
//! Transport adapters deserialize requests and render results only. This owner
//! performs validation, revision CAS, persistence, broker activation, refresh,
//! durable status, and forward rollback.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::RwLock;
use tracedecay_domain::{ManifestDigest, ProjectId, UtcMicros, canonical_sha256};

use crate::db::Database;
use crate::diagnostics::lsp::activity::{active_languages_for_files, documents_for_adapter};
use crate::diagnostics::lsp::adapters::LspAdapterDefinition;
use crate::diagnostics::lsp::broker::{
    DiagnosticBroker, DiagnosticsSnapshot, EngineState, NodeSpan,
};
use crate::diagnostics::lsp::settings::{CodeDiagnosticsSettings, IdleBackfillMode, save_settings};
use crate::errors::{Result, TraceDecayError};

const RECEIPT_DIRECTORY: &str = "code-diagnostics-operations";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeDiagnosticsSettingsPatchV1 {
    pub expected_revision: ManifestDigest,
    #[serde(default)]
    pub idle_backfill: Option<IdleBackfillMode>,
    #[serde(default)]
    pub languages: BTreeMap<String, LanguageSettingsPatchV1>,
    #[serde(default)]
    pub custom_adapters: Option<Vec<LspAdapterDefinition>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LanguageSettingsPatchV1 {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_command_override_patch")]
    pub command_override: CommandOverridePatchV1,
}

#[derive(Debug, Clone, Default)]
pub enum CommandOverridePatchV1 {
    #[default]
    Missing,
    Null,
    Value(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeDiagnosticsRefreshTargetV1 {
    All,
    Language(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodeDiagnosticsSettingsPreviewV1 {
    pub preview_id: ManifestDigest,
    pub project_id: ProjectId,
    pub base_revision: ManifestDigest,
    pub candidate_revision: ManifestDigest,
    pub previous: CodeDiagnosticsSettings,
    pub candidate: CodeDiagnosticsSettings,
    pub changed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodeDiagnosticsRefreshPreviewV1 {
    pub preview_id: ManifestDigest,
    pub project_id: ProjectId,
    pub settings_revision: ManifestDigest,
    pub target: CodeDiagnosticsRefreshTargetV1,
    pub languages: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeDiagnosticsOperationKindV1 {
    SettingsApply,
    SettingsRollback,
    Refresh,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodeDiagnosticsOperationReceiptV1 {
    pub operation_id: ManifestDigest,
    pub project_id: ProjectId,
    pub kind: CodeDiagnosticsOperationKindV1,
    pub preview_id: ManifestDigest,
    pub base_revision: ManifestDigest,
    pub result_revision: ManifestDigest,
    pub completed_at: UtcMicros,
    pub refreshed_languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_settings: Option<CodeDiagnosticsSettings>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CodeDiagnosticsOperationStatusV1 {
    pub receipt: CodeDiagnosticsOperationReceiptV1,
    pub current_revision: ManifestDigest,
    pub is_current: bool,
}

#[derive(Clone)]
pub struct CodeDiagnosticsControl {
    project_id: ProjectId,
    project_root: PathBuf,
    dashboard_root: PathBuf,
    graph: Arc<Database>,
    broker: Arc<RwLock<DiagnosticBroker>>,
}

impl CodeDiagnosticsControl {
    pub fn new(
        project_id: ProjectId,
        project_root: PathBuf,
        dashboard_root: PathBuf,
        graph: Arc<Database>,
        broker: Arc<RwLock<DiagnosticBroker>>,
    ) -> Self {
        Self {
            project_id,
            project_root,
            dashboard_root,
            graph,
            broker,
        }
    }

    pub async fn snapshot(&self) -> Result<DiagnosticsSnapshot> {
        self.reconcile_project_language_activity().await?;
        Ok(self.broker.read().await.snapshot())
    }

    pub async fn preview_settings(
        &self,
        patch: CodeDiagnosticsSettingsPatchV1,
    ) -> Result<CodeDiagnosticsSettingsPreviewV1> {
        let previous = self.broker.read().await.snapshot().settings;
        let base_revision = settings_revision(&previous)?;
        if patch.expected_revision != base_revision {
            return Err(config_error("code diagnostics settings revision conflict"));
        }
        let candidate = apply_settings_patch(previous.clone(), patch)?;
        validate_settings(&candidate)?;
        let candidate_revision = settings_revision(&candidate)?;
        let preview_id =
            settings_preview_identity(&self.project_id, &base_revision, &candidate_revision)?;
        Ok(CodeDiagnosticsSettingsPreviewV1 {
            preview_id,
            project_id: self.project_id.clone(),
            base_revision,
            changed: candidate != previous,
            candidate_revision,
            previous,
            candidate,
        })
    }

    pub async fn apply_settings(
        &self,
        mut preview: CodeDiagnosticsSettingsPreviewV1,
    ) -> Result<CodeDiagnosticsOperationReceiptV1> {
        self.validate_preview_project(&preview.project_id)?;
        let mut broker = self.broker.write().await;
        let current = broker.snapshot().settings;
        let current_revision = settings_revision(&current)?;
        if current_revision != preview.base_revision {
            return Err(config_error("code diagnostics settings preview is stale"));
        }
        if settings_revision(&preview.previous)? != preview.base_revision
            || settings_revision(&preview.candidate)? != preview.candidate_revision
        {
            return Err(config_error("code diagnostics settings preview is invalid"));
        }
        validate_settings(&preview.candidate)?;
        let expected_preview_id = settings_preview_identity(
            &self.project_id,
            &preview.base_revision,
            &preview.candidate_revision,
        )?;
        if preview.preview_id != expected_preview_id {
            return Err(config_error("code diagnostics settings preview is invalid"));
        }
        preview.changed = preview.candidate != preview.previous;
        if preview.changed {
            save_settings(&self.dashboard_root, &preview.candidate).await?;
            activate_broker_settings(&mut broker, preview.candidate.clone());
        }
        drop(broker);
        let receipt = self
            .receipt(
                CodeDiagnosticsOperationKindV1::SettingsApply,
                preview.preview_id,
                preview.base_revision,
                preview.candidate_revision,
                Vec::new(),
                Some(preview.previous),
            )
            .await?;
        self.persist_receipt(&receipt).await?;
        Ok(receipt)
    }

    pub async fn rollback_settings(
        &self,
        receipt: &CodeDiagnosticsOperationReceiptV1,
        expected_revision: ManifestDigest,
    ) -> Result<CodeDiagnosticsOperationReceiptV1> {
        self.validate_preview_project(&receipt.project_id)?;
        if receipt_identity(receipt)? != receipt.operation_id {
            return Err(config_error(
                "code diagnostics operation receipt identity is invalid",
            ));
        }
        if receipt.kind != CodeDiagnosticsOperationKindV1::SettingsApply {
            return Err(config_error(
                "only a completed settings apply can be rolled back",
            ));
        }
        let rollback = receipt
            .rollback_settings
            .clone()
            .ok_or_else(|| config_error("settings receipt has no rollback state"))?;
        let mut broker = self.broker.write().await;
        let current = broker.snapshot().settings;
        let current_revision = settings_revision(&current)?;
        if current_revision != expected_revision || current_revision != receipt.result_revision {
            return Err(config_error("code diagnostics rollback revision conflict"));
        }
        let result_revision = settings_revision(&rollback)?;
        save_settings(&self.dashboard_root, &rollback).await?;
        activate_broker_settings(&mut broker, rollback);
        drop(broker);
        let rollback_preview_id = canonical_sha256(&(
            "tracedecay.code-diagnostics.settings-rollback.v1",
            &receipt.operation_id,
            &expected_revision,
            &result_revision,
        ))
        .map_err(domain_error)?;
        let rollback_receipt = self
            .receipt(
                CodeDiagnosticsOperationKindV1::SettingsRollback,
                rollback_preview_id,
                current_revision,
                result_revision,
                Vec::new(),
                Some(current),
            )
            .await?;
        self.persist_receipt(&rollback_receipt).await?;
        Ok(rollback_receipt)
    }

    pub async fn preview_refresh(
        &self,
        target: CodeDiagnosticsRefreshTargetV1,
    ) -> Result<CodeDiagnosticsRefreshPreviewV1> {
        let snapshot = self.snapshot().await?;
        let settings_revision = settings_revision(&snapshot.settings)?;
        let languages = refresh_languages_for_target(&snapshot, &target)?;
        let preview_id = canonical_sha256(&(
            "tracedecay.code-diagnostics.refresh-preview.v1",
            &self.project_id,
            &settings_revision,
            &target,
            &languages,
        ))
        .map_err(domain_error)?;
        Ok(CodeDiagnosticsRefreshPreviewV1 {
            preview_id,
            project_id: self.project_id.clone(),
            settings_revision,
            target,
            languages,
        })
    }

    pub async fn apply_refresh(
        &self,
        preview: CodeDiagnosticsRefreshPreviewV1,
    ) -> Result<CodeDiagnosticsOperationReceiptV1> {
        self.validate_preview_project(&preview.project_id)?;
        let current = self.broker.read().await.snapshot().settings;
        let current_revision = settings_revision(&current)?;
        if current_revision != preview.settings_revision {
            return Err(config_error("code diagnostics refresh preview is stale"));
        }
        let expected_preview_id = canonical_sha256(&(
            "tracedecay.code-diagnostics.refresh-preview.v1",
            &self.project_id,
            &preview.settings_revision,
            &preview.target,
            &preview.languages,
        ))
        .map_err(domain_error)?;
        if expected_preview_id != preview.preview_id {
            return Err(config_error("code diagnostics refresh preview is invalid"));
        }
        let snapshot = self.snapshot().await?;
        let expected_languages = refresh_languages_for_target(&snapshot, &preview.target)?;
        if expected_languages != preview.languages {
            return Err(config_error("code diagnostics refresh preview is stale"));
        }
        for language in &preview.languages {
            self.refresh_one(language).await?;
        }
        let result_revision = diagnostics_revision(&self.broker.read().await.snapshot())?;
        let receipt = self
            .receipt(
                CodeDiagnosticsOperationKindV1::Refresh,
                preview.preview_id,
                current_revision,
                result_revision,
                preview.languages,
                None,
            )
            .await?;
        self.persist_receipt(&receipt).await?;
        Ok(receipt)
    }

    pub async fn status(
        &self,
        operation_id: &ManifestDigest,
    ) -> Result<CodeDiagnosticsOperationStatusV1> {
        let path = receipt_path(&self.dashboard_root, operation_id)?;
        let bytes = tokio::fs::read(&path).await.map_err(|error| {
            config_error(format!(
                "failed to read code diagnostics operation '{}': {error}",
                path.display()
            ))
        })?;
        let receipt: CodeDiagnosticsOperationReceiptV1 =
            serde_json::from_slice(&bytes).map_err(|error| {
                config_error(format!(
                    "failed to parse code diagnostics operation '{}': {error}",
                    path.display()
                ))
            })?;
        self.validate_preview_project(&receipt.project_id)?;
        if &receipt.operation_id != operation_id {
            return Err(config_error(
                "code diagnostics operation receipt identity is invalid",
            ));
        }
        if receipt_identity(&receipt)? != receipt.operation_id {
            return Err(config_error(
                "code diagnostics operation receipt is invalid",
            ));
        }
        let snapshot = self.broker.read().await.snapshot();
        let current_revision = match receipt.kind {
            CodeDiagnosticsOperationKindV1::Refresh => diagnostics_revision(&snapshot)?,
            CodeDiagnosticsOperationKindV1::SettingsApply
            | CodeDiagnosticsOperationKindV1::SettingsRollback => {
                settings_revision(&snapshot.settings)?
            }
        };
        Ok(CodeDiagnosticsOperationStatusV1 {
            is_current: current_revision == receipt.result_revision,
            receipt,
            current_revision,
        })
    }

    async fn refresh_one(&self, language: &str) -> Result<()> {
        let snapshot = self.broker.read().await.snapshot();
        if !snapshot.settings.language_enabled(language) {
            return Err(config_error("code diagnostics language is disabled"));
        }
        let adapter = self
            .broker
            .read()
            .await
            .adapter_for(language)
            .ok_or_else(|| config_error("code diagnostics adapter is unavailable"))?;
        let files = indexed_files(&self.graph).await?;
        let documents = documents_for_adapter(&self.project_root, &adapter, files).await?;
        let document_count = documents.len();
        self.broker.write().await.record_backfill_progress(
            language,
            document_count,
            document_count,
            0,
            None,
        );
        if documents.is_empty() {
            self.broker.write().await.record_backfill_progress(
                language,
                0,
                0,
                0,
                Some(crate::tracedecay::current_timestamp()),
            );
            return Ok(());
        }
        let prepared = self
            .broker
            .write()
            .await
            .prepare_refresh(language, documents)?;
        let refresh_ok = match prepared {
            Some(prepared) => {
                let completed = prepared.collect_diagnostics(Duration::from_secs(5)).await;
                let ok = completed.is_ok();
                let mut broker = self.broker.write().await;
                broker.finish_refresh(completed)?;
                let graph = Arc::clone(&self.graph);
                broker
                    .resolve_enclosing_nodes(move |file| {
                        let graph = Arc::clone(&graph);
                        async move { node_spans_for_file(&graph, &file).await }
                    })
                    .await;
                let snapshot = broker.snapshot();
                broker.record_backfill_progress(
                    language,
                    document_count,
                    document_count,
                    files_with_diagnostics(&snapshot, language),
                    ok.then(crate::tracedecay::current_timestamp),
                );
                ok
            }
            None => true,
        };
        if !refresh_ok {
            return Err(config_error("code diagnostics refresh failed"));
        }
        Ok(())
    }

    async fn reconcile_project_language_activity(&self) -> Result<()> {
        let files = indexed_files(&self.graph).await?;
        let adapters = {
            let broker = self.broker.read().await;
            broker
                .snapshot()
                .engines
                .into_iter()
                .filter_map(|engine| broker.adapter_for(&engine.language))
                .collect::<Vec<_>>()
        };
        let active = active_languages_for_files(&self.project_root, &adapters, &files);
        self.broker.write().await.update_project_languages(active);
        Ok(())
    }

    async fn receipt(
        &self,
        kind: CodeDiagnosticsOperationKindV1,
        preview_id: ManifestDigest,
        base_revision: ManifestDigest,
        result_revision: ManifestDigest,
        refreshed_languages: Vec<String>,
        rollback_settings: Option<CodeDiagnosticsSettings>,
    ) -> Result<CodeDiagnosticsOperationReceiptV1> {
        let completed_at = current_micros()?;
        let mut receipt = CodeDiagnosticsOperationReceiptV1 {
            operation_id: canonical_sha256(&"tracedecay.code-diagnostics.operation.pending")
                .map_err(domain_error)?,
            project_id: self.project_id.clone(),
            kind,
            preview_id,
            base_revision,
            result_revision,
            completed_at,
            refreshed_languages,
            rollback_settings,
        };
        receipt.operation_id = receipt_identity(&receipt)?;
        Ok(receipt)
    }

    async fn persist_receipt(&self, receipt: &CodeDiagnosticsOperationReceiptV1) -> Result<()> {
        let path = receipt_path(&self.dashboard_root, &receipt.operation_id)?;
        let parent = path
            .parent()
            .ok_or_else(|| config_error("code diagnostics operation path has no parent"))?;
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            config_error(format!(
                "failed to create code diagnostics operation directory '{}': {error}",
                parent.display()
            ))
        })?;
        let bytes = serde_json::to_vec_pretty(receipt).map_err(|error| {
            config_error(format!("failed to encode operation receipt: {error}"))
        })?;
        let temporary = path.with_extension("json.pending");
        crate::db::DatabaseAuthority::publish_record_atomically(
            &temporary,
            &path,
            &bytes,
            "code diagnostics operation receipt",
        )
    }

    fn validate_preview_project(&self, project_id: &ProjectId) -> Result<()> {
        if project_id != &self.project_id {
            return Err(config_error(
                "code diagnostics operation is not authorized for this project",
            ));
        }
        Ok(())
    }
}

pub fn settings_revision(settings: &CodeDiagnosticsSettings) -> Result<ManifestDigest> {
    canonical_sha256(&("tracedecay.code-diagnostics.settings.v1", settings)).map_err(domain_error)
}

fn diagnostics_revision(snapshot: &DiagnosticsSnapshot) -> Result<ManifestDigest> {
    canonical_sha256(&(
        "tracedecay.code-diagnostics.result.v1",
        &snapshot.settings,
        &snapshot.diagnostics,
        &snapshot.backfill,
    ))
    .map_err(domain_error)
}

fn settings_preview_identity(
    project_id: &ProjectId,
    base_revision: &ManifestDigest,
    candidate_revision: &ManifestDigest,
) -> Result<ManifestDigest> {
    canonical_sha256(&(
        "tracedecay.code-diagnostics.settings-preview.v1",
        project_id,
        base_revision,
        candidate_revision,
    ))
    .map_err(domain_error)
}

fn receipt_identity(receipt: &CodeDiagnosticsOperationReceiptV1) -> Result<ManifestDigest> {
    canonical_sha256(&(
        "tracedecay.code-diagnostics.operation.v1",
        &receipt.project_id,
        receipt.kind,
        &receipt.preview_id,
        &receipt.base_revision,
        &receipt.result_revision,
        receipt.completed_at,
        &receipt.refreshed_languages,
        &receipt.rollback_settings,
    ))
    .map_err(domain_error)
}

fn apply_settings_patch(
    mut settings: CodeDiagnosticsSettings,
    patch: CodeDiagnosticsSettingsPatchV1,
) -> Result<CodeDiagnosticsSettings> {
    if let Some(mode) = patch.idle_backfill {
        settings.idle_backfill = mode;
    }
    for (language, language_patch) in patch.languages {
        validate_label(&language, "diagnostic language")?;
        let language_settings = settings.languages.entry(language).or_default();
        if let Some(enabled) = language_patch.enabled {
            language_settings.enabled = enabled;
        }
        match language_patch.command_override {
            CommandOverridePatchV1::Missing => {}
            CommandOverridePatchV1::Null => language_settings.command_override = None,
            CommandOverridePatchV1::Value(command) => {
                validate_label(&command, "diagnostic command override")?;
                language_settings.command_override = Some(command);
            }
        }
    }
    if let Some(custom_adapters) = patch.custom_adapters {
        settings.custom_adapters = custom_adapters;
    }
    Ok(settings)
}

fn activate_broker_settings(broker: &mut DiagnosticBroker, settings: CodeDiagnosticsSettings) {
    let mut adapters = crate::diagnostics::lsp::adapters::builtin_adapters();
    adapters.extend(settings.custom_adapters.clone());
    broker.update_adapters(adapters);
    broker.update_settings(settings);
}

fn validate_settings(settings: &CodeDiagnosticsSettings) -> Result<()> {
    let mut languages = BTreeSet::new();
    for adapter in &settings.custom_adapters {
        for (value, field) in [
            (&adapter.language, "diagnostic adapter language"),
            (&adapter.language_id, "diagnostic adapter language id"),
            (&adapter.command, "diagnostic adapter command"),
        ] {
            validate_label(value, field)?;
        }
        if !languages.insert(adapter.language.as_str()) {
            return Err(config_error(
                "custom diagnostic adapter languages must be unique",
            ));
        }
    }
    Ok(())
}

fn refreshable_languages(snapshot: &DiagnosticsSnapshot) -> Vec<String> {
    snapshot
        .engines
        .iter()
        .filter(|engine| {
            engine.enabled
                && !matches!(
                    engine.state,
                    EngineState::Disabled | EngineState::Inactive | EngineState::Unavailable
                )
        })
        .map(|engine| engine.language.clone())
        .collect()
}

fn refresh_languages_for_target(
    snapshot: &DiagnosticsSnapshot,
    target: &CodeDiagnosticsRefreshTargetV1,
) -> Result<Vec<String>> {
    let available = refreshable_languages(snapshot);
    match target {
        CodeDiagnosticsRefreshTargetV1::All => Ok(available),
        CodeDiagnosticsRefreshTargetV1::Language(language) => {
            validate_label(language, "diagnostic language")?;
            if !snapshot.settings.language_enabled(language) {
                return Err(config_error("code diagnostics language is disabled"));
            }
            if !available.iter().any(|candidate| candidate == language) {
                return Err(config_error("code diagnostics language is not refreshable"));
            }
            Ok(vec![language.clone()])
        }
    }
}

fn files_with_diagnostics(snapshot: &DiagnosticsSnapshot, language: &str) -> usize {
    snapshot
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.language == language)
        .map(|diagnostic| diagnostic.file.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

async fn node_spans_for_file(db: &Database, file: &str) -> Vec<NodeSpan> {
    db.get_nodes_by_file(file)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|node| NodeSpan {
            start_line: node.start_line,
            end_line: node.end_line,
            qualified_name: node.qualified_name,
        })
        .collect()
}

async fn indexed_files(db: &Database) -> Result<Vec<String>> {
    let mut files = db
        .get_all_files()
        .await?
        .into_iter()
        .map(|file| file.path)
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn receipt_path(root: &Path, operation_id: &ManifestDigest) -> Result<PathBuf> {
    let digest = operation_id
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| config_error("code diagnostics operation identity is invalid"))?;
    Ok(root.join(RECEIPT_DIRECTORY).join(format!("{digest}.json")))
}

fn validate_label(value: &str, field: &'static str) -> Result<()> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        return Err(config_error(format!("{field} is invalid")));
    }
    Ok(())
}

fn deserialize_command_override_patch<'de, D>(
    deserializer: D,
) -> std::result::Result<CommandOverridePatchV1, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match Option::<String>::deserialize(deserializer)? {
        Some(value) => CommandOverridePatchV1::Value(value),
        None => CommandOverridePatchV1::Null,
    })
}

fn current_micros() -> Result<UtcMicros> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| config_error("system time is before the Unix epoch"))?;
    Ok(UtcMicros(
        i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX),
    ))
}

fn domain_error(error: impl std::fmt::Display) -> TraceDecayError {
    config_error(error.to_string())
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseAuthority, TestDatabaseRuntimeMode};

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    #[test]
    fn settings_patch_preserves_omitted_fields() {
        let settings = CodeDiagnosticsSettings::default();
        let result = apply_settings_patch(
            settings,
            CodeDiagnosticsSettingsPatchV1 {
                expected_revision: digest('a'),
                languages: BTreeMap::from([(
                    "rust".to_owned(),
                    LanguageSettingsPatchV1 {
                        enabled: Some(false),
                        command_override: CommandOverridePatchV1::Missing,
                    },
                )]),
                idle_backfill: None,
                custom_adapters: None,
            },
        )
        .unwrap();
        assert!(!result.language_enabled("rust"));
    }

    #[test]
    fn invalid_custom_adapter_is_rejected_by_application_validation() {
        let mut settings = CodeDiagnosticsSettings::default();
        let mut adapter = crate::diagnostics::lsp::adapters::builtin_adapters()
            .into_iter()
            .next()
            .unwrap();
        adapter.command.clear();
        settings.custom_adapters.push(adapter);
        assert!(validate_settings(&settings).is_err());
    }

    #[tokio::test]
    async fn settings_apply_status_stale_cas_and_forward_rollback_are_exact() {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("graph.db");
        let authority =
            DatabaseAuthority::acquire_test(&database_path, "diagnostics control test").unwrap();
        let (database, _) = Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let settings = CodeDiagnosticsSettings::default();
        let broker = DiagnosticBroker::new(
            temp.path().to_path_buf(),
            crate::diagnostics::lsp::adapters::builtin_adapters(),
            settings.clone(),
        );
        let control = CodeDiagnosticsControl::new(
            ProjectId::new("project.diagnostics-control").unwrap(),
            temp.path().to_path_buf(),
            temp.path().join("dashboard"),
            Arc::new(database),
            Arc::new(RwLock::new(broker)),
        );
        let base_revision = settings_revision(&settings).unwrap();
        let preview = control
            .preview_settings(CodeDiagnosticsSettingsPatchV1 {
                expected_revision: base_revision.clone(),
                idle_backfill: Some(IdleBackfillMode::Off),
                languages: BTreeMap::new(),
                custom_adapters: None,
            })
            .await
            .unwrap();
        let stale_preview = preview.clone();
        let receipt = control.apply_settings(preview).await.unwrap();
        assert_ne!(receipt.result_revision, base_revision);
        let status = control.status(&receipt.operation_id).await.unwrap();
        assert!(status.is_current);
        assert_eq!(status.receipt.operation_id, receipt.operation_id);
        assert!(control.apply_settings(stale_preview).await.is_err());

        let mut tampered_receipt = receipt.clone();
        tampered_receipt.rollback_settings = Some({
            let mut settings = settings.clone();
            settings.idle_backfill = IdleBackfillMode::Off;
            settings
        });
        assert!(
            control
                .rollback_settings(&tampered_receipt, receipt.result_revision.clone())
                .await
                .is_err()
        );

        let rollback = control
            .rollback_settings(&receipt, receipt.result_revision.clone())
            .await
            .unwrap();
        assert_eq!(rollback.result_revision, base_revision);
        assert!(
            !control
                .status(&receipt.operation_id)
                .await
                .unwrap()
                .is_current
        );
    }
}
