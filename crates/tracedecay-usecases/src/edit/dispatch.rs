use tracedecay_application::{
    ApiMigrationPlanRequestV1, ApiMigrationPlanV1, CancellationStage, SourceEditRequest,
};

use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::errors::Result;

use super::control::SourceEditEffectControlV1;
use super::outcome::SourceEditOutcome;
use super::verify::config_error;

pub(super) async fn run_source_edit(
    graph: &TraceDecay,
    request: SourceEditRequest,
    control: Option<&SourceEditEffectControlV1>,
) -> Result<SourceEditOutcome> {
    Ok(match request {
        SourceEditRequest::StrReplace {
            path,
            old_str,
            new_str,
            dry_run,
            ..
        } => SourceEditOutcome::Edit(
            graph
                .str_replace(&path, &old_str, &new_str, dry_run)
                .await?,
        ),
        SourceEditRequest::MultiStrReplace {
            path,
            replacements,
            dry_run,
            ..
        } => {
            let replacements = replacements
                .iter()
                .map(|(old, new)| (old.as_str(), new.as_str()))
                .collect::<Vec<_>>();
            SourceEditOutcome::MultiEdit(
                graph
                    .multi_str_replace(&path, &replacements, dry_run)
                    .await?,
            )
        }
        SourceEditRequest::InsertAt {
            path,
            anchor,
            content,
            before,
            dry_run,
            ..
        } => SourceEditOutcome::Insert(
            graph
                .insert_at(&path, &anchor, &content, before, dry_run)
                .await?,
        ),
        SourceEditRequest::AstGrepRewrite {
            path,
            pattern,
            rewrite,
            dry_run,
            ..
        } => SourceEditOutcome::AstGrep(
            graph
                .ast_grep_rewrite(&path, &pattern, &rewrite, dry_run)
                .await?,
        ),
        SourceEditRequest::ReplaceSymbol {
            symbol,
            new_source,
            dry_run,
            ..
        } => SourceEditOutcome::Edit(graph.replace_symbol(&symbol, &new_source, dry_run).await?),
        SourceEditRequest::InsertAtSymbol {
            symbol,
            content,
            position,
            dry_run,
            ..
        } => SourceEditOutcome::Insert(
            graph
                .insert_at_symbol(&symbol, &content, &position, dry_run)
                .await?,
        ),
        SourceEditRequest::MoveSymbol {
            symbol,
            dest_file,
            dry_run,
            update_references,
        } => SourceEditOutcome::Move(
            graph
                .move_symbol(&symbol, &dest_file, dry_run, update_references)
                .await?,
        ),
        SourceEditRequest::ApiMigrationApply {
            plan,
            plan_digest,
            dry_run,
            ..
        } => {
            if plan.plan_digest != plan_digest {
                return Err(config_error(
                    "API migration apply digest does not match its immutable plan",
                ));
            }
            let replanned = crate::api_migration::plan_api_migration(
                graph,
                ApiMigrationPlanRequestV1 {
                    family_id: plan.family_id.clone(),
                    operations: plan.operations.clone(),
                },
            )
            .await?;
            validate_replanned_api_migration(&plan, &replanned)?;
            let mut is_cancelled = || {
                control
                    .and_then(|control| control.checkpoint(CancellationStage::EffectInFlight))
                    .is_some()
            };
            SourceEditOutcome::ApiMigration(
                graph
                    .apply_api_migration_plan(&replanned, dry_run, &mut is_cancelled)
                    .await?,
            )
        }
    })
}

fn validate_replanned_api_migration(
    supplied: &ApiMigrationPlanV1,
    replanned: &ApiMigrationPlanV1,
) -> Result<()> {
    if supplied != replanned {
        return Err(config_error(
            "API migration plan does not match current graph-backed evidence; replan before apply",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::test_support::*;

    use std::fs;
    use tempfile::tempdir;
    use tracedecay_application::{
        ApiCompatibilityDispositionV1, ApiCompatibilityLifetimeV1, ApiDefinitionInsertionV1,
        ApiMigrationOperationRequestV1, ApiMigrationPlanRequestV1, ApiMigrationPlanV1,
        ApiMigrationSiteDispositionV1, api_migration_definition_digest,
    };
    use tracedecay_domain::ManifestDigest;

    #[tokio::test]
    async fn api_migration_promote_primary_plans_and_applies_the_replacement_definition() {
        let initial = "pub fn legacy_api() -> &'static str {\n    \"legacy\"\n}\n";
        let expected = "pub fn primary_api() -> &'static str {\n    \"primary\"\n}\n";
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let operation = ApiMigrationOperationRequestV1::PromotePrimary {
            operation_id: "promote-primary".to_owned(),
            depends_on: Vec::new(),
            symbol: api_migration_symbol(&graph, "legacy_api").await,
            expected_definition_digest: api_migration_definition_digest(initial).unwrap(),
            replacement_definition: expected.to_owned(),
        };

        let plan = plan_api_migration_fixture(&graph, "family.promote-primary", operation).await;

        assert!(!plan.blocked);
        assert_eq!(plan.sites.len(), 1);
        assert_eq!(plan.sites[0].reason, "whole definition replacement");
        assert_eq!(plan.files[0].intended_content, expected);
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);
        assert_eq!(result.changed_sites, 1);
        assert_eq!(result.changed_files, ["src/lib.rs"]);
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn api_migration_replace_definition_replans_rejects_stale_bytes_then_applies_current_plan()
     {
        let initial = "pub fn current_value() -> i32 {\n    1\n}\n";
        let concurrent = "pub fn current_value() -> i32 {\n    2\n}\n";
        let expected = "pub fn current_value() -> i32 {\n    3\n}\n";
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let operation = ApiMigrationOperationRequestV1::ReplaceDefinition {
            operation_id: "replace-definition".to_owned(),
            depends_on: Vec::new(),
            symbol: api_migration_symbol(&graph, "current_value").await,
            expected_definition_digest: api_migration_definition_digest(initial).unwrap(),
            replacement_definition: expected.to_owned(),
        };
        let plan = plan_api_migration_fixture(&graph, "family.replace-definition", operation).await;
        assert_eq!(plan.files[0].intended_content, expected);

        fs::write(project.path().join("src/lib.rs"), concurrent).unwrap();
        let stale_error = run_source_edit(
            &graph,
            SourceEditRequest::ApiMigrationApply {
                plan: plan.clone(),
                plan_digest: plan.plan_digest.clone(),
                dry_run: false,
                verify: false,
            },
            None,
        )
        .await
        .unwrap_err();

        assert!(
            stale_error
                .to_string()
                .contains("plan does not match current graph-backed evidence; replan before apply")
        );
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            concurrent
        );
        let replanned = crate::api_migration::plan_api_migration(
            &graph,
            ApiMigrationPlanRequestV1 {
                family_id: plan.family_id.clone(),
                operations: plan.operations.clone(),
            },
        )
        .await
        .unwrap();
        assert!(replanned.blocked);
        assert_ne!(replanned.plan_digest, plan.plan_digest);
        assert_eq!(
            replanned.files[0].expected_content,
            replanned.files[0].intended_content
        );
        fs::write(project.path().join("src/lib.rs"), initial).unwrap();
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);
        assert_eq!(result.changed_sites, 1);
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn api_migration_rename_bound_symbol_plans_and_applies_declaration_and_caller_sites() {
        let initial = "pub fn legacy_name() -> i32 {\n    1\n}\n\npub fn caller() -> i32 {\n    legacy_name()\n}\n";
        let expected = "pub fn current_name() -> i32 {\n    1\n}\n\npub fn caller() -> i32 {\n    current_name()\n}\n";
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let operation = ApiMigrationOperationRequestV1::RenameBoundSymbol {
            operation_id: "rename-bound-symbol".to_owned(),
            depends_on: Vec::new(),
            symbol: api_migration_symbol(&graph, "legacy_name").await,
            new_name: "current_name".to_owned(),
        };

        let plan =
            plan_api_migration_fixture(&graph, "family.rename-bound-symbol", operation).await;

        assert!(!plan.blocked);
        assert_eq!(plan.sites.len(), 2);
        assert!(
            plan.sites
                .iter()
                .all(|site| { site.disposition == ApiMigrationSiteDispositionV1::Changed })
        );
        assert!(
            plan.sites
                .iter()
                .any(|site| site.reason == "bound declaration rename")
        );
        assert!(
            plan.sites
                .iter()
                .any(|site| site.reason == "graph-bound caller rename")
        );
        assert_eq!(plan.files[0].intended_content, expected);
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);
        assert_eq!(result.changed_sites, 2);
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn api_migration_insert_compatibility_plans_and_applies_the_definition_after_its_anchor()
    {
        let initial = "pub fn current_api() -> i32 {\n    7\n}\n";
        let compatibility = "#[deprecated]\npub fn legacy_api() -> i32 {\n    current_api()\n}";
        let expected = format!("{initial}\n{compatibility}");
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let operation = ApiMigrationOperationRequestV1::InsertCompatibility {
            operation_id: "insert-compatibility".to_owned(),
            depends_on: Vec::new(),
            anchor: api_migration_symbol(&graph, "current_api").await,
            position: ApiDefinitionInsertionV1::After,
            definition: compatibility.to_owned(),
            disposition: ApiCompatibilityDispositionV1 {
                lifetime: ApiCompatibilityLifetimeV1::StablePublicContract,
                external_consumer: "fixture consumer".to_owned(),
                owner: "fixture API team".to_owned(),
                deprecation_policy: "retained as a stable compatibility alias".to_owned(),
                deletion_condition: None,
            },
        };

        let plan =
            plan_api_migration_fixture(&graph, "family.insert-compatibility", operation).await;

        assert!(!plan.blocked);
        assert_eq!(plan.sites.len(), 1);
        assert_eq!(plan.sites[0].reason, "deliberate compatibility definition");
        assert_eq!(plan.files[0].intended_content, expected);
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);
        assert_eq!(result.compatibility_sites, 1);
        assert_eq!(result.changed_sites, 1);
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn api_migration_replace_selected_terminology_plans_and_applies_only_selected_ast_occurrences()
     {
        let initial = "pub fn terminology() -> i32 {\n    let legacy = 1;\n    legacy\n}\n";
        let expected = "pub fn terminology() -> i32 {\n    let current = 1;\n    current\n}\n";
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let operation = ApiMigrationOperationRequestV1::ReplaceSelectedTerminology {
            operation_id: "replace-selected-terminology".to_owned(),
            depends_on: Vec::new(),
            enclosing_symbol: api_migration_symbol(&graph, "terminology").await,
            old_term: "legacy".to_owned(),
            new_term: "current".to_owned(),
            occurrence_indexes: vec![0, 1],
        };

        let plan =
            plan_api_migration_fixture(&graph, "family.replace-selected-terminology", operation)
                .await;

        assert!(!plan.blocked);
        assert_eq!(plan.sites.len(), 2);
        assert!(plan.sites.iter().all(|site| {
            site.reason == "selected AST terminology replacement"
                && site.disposition == ApiMigrationSiteDispositionV1::Changed
        }));
        assert_eq!(plan.files[0].intended_content, expected);
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);
        assert_eq!(result.changed_sites, 2);
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn api_migration_assert_stable_value_plans_and_applies_a_byte_identical_protected_site() {
        let initial = "pub fn stable_value() -> i32 {\n    42\n}\n";
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let operation = ApiMigrationOperationRequestV1::AssertStableValue {
            operation_id: "assert-stable-value".to_owned(),
            depends_on: Vec::new(),
            enclosing_symbol: api_migration_symbol(&graph, "stable_value").await,
            category: "wire discriminant".to_owned(),
            exact_bytes: "42".to_owned(),
            occurrence_indexes: vec![0],
        };

        let plan =
            plan_api_migration_fixture(&graph, "family.assert-stable-value", operation).await;

        assert!(!plan.blocked);
        assert_eq!(plan.sites.len(), 1);
        assert_eq!(
            plan.sites[0].disposition,
            ApiMigrationSiteDispositionV1::Skipped
        );
        assert_eq!(
            plan.sites[0].reason,
            "protected wire discriminant remains byte-identical"
        );
        assert_eq!(
            plan.files[0].expected_content,
            plan.files[0].intended_content
        );
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);
        assert_eq!(result.changed_sites, 0);
        assert_eq!(result.protected_values_verified, 1);
        assert!(result.changed_files.is_empty());
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            initial
        );
    }

    #[tokio::test]
    async fn api_migration_replan_classifies_applied_compatibility_and_terminology_as_unchanged() {
        let initial = "pub fn current_api() -> i32 {\n    7\n}\n\npub fn terminology() -> i32 {\n    let legacy = 1;\n    legacy\n}\n";
        let compatibility = "#[deprecated]\npub fn legacy_api() -> i32 {\n    current_api()\n}";
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let request = ApiMigrationPlanRequestV1 {
            family_id: "family.replan".to_owned(),
            operations: vec![
                ApiMigrationOperationRequestV1::InsertCompatibility {
                    operation_id: "insert-compatibility".to_owned(),
                    depends_on: Vec::new(),
                    anchor: api_migration_symbol(&graph, "current_api").await,
                    position: ApiDefinitionInsertionV1::After,
                    definition: compatibility.to_owned(),
                    disposition: ApiCompatibilityDispositionV1 {
                        lifetime: ApiCompatibilityLifetimeV1::StablePublicContract,
                        external_consumer: "fixture consumer".to_owned(),
                        owner: "fixture API team".to_owned(),
                        deprecation_policy: "retained as a stable compatibility alias".to_owned(),
                        deletion_condition: None,
                    },
                },
                ApiMigrationOperationRequestV1::ReplaceSelectedTerminology {
                    operation_id: "replace-terminology".to_owned(),
                    depends_on: Vec::new(),
                    enclosing_symbol: api_migration_symbol(&graph, "terminology").await,
                    old_term: "legacy".to_owned(),
                    new_term: "current".to_owned(),
                    occurrence_indexes: vec![0, 1],
                },
            ],
        };
        let plan = crate::api_migration::plan_api_migration(&graph, request.clone())
            .await
            .unwrap();
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);

        let replanned = crate::api_migration::plan_api_migration(&graph, request)
            .await
            .unwrap();

        assert!(!replanned.blocked);
        assert!(replanned.files.iter().all(|file| {
            file.expected_content == file.intended_content
                && fs::read_to_string(project.path().join(&file.path)).unwrap()
                    == file.expected_content
        }));
        assert!(replanned.sites.iter().all(|site| {
            site.disposition == ApiMigrationSiteDispositionV1::Unchanged
                || site.disposition == ApiMigrationSiteDispositionV1::Skipped
        }));
    }

    #[tokio::test]
    async fn api_migration_blocks_a_definition_replacement_that_changes_protected_bytes() {
        let initial = "pub fn wire_name() -> &'static str {\n    \"stable_name\"\n}\n";
        let replacement = "pub fn wire_name() -> &'static str {\n    \"changed_name\"\n}\n";
        let (_project, graph) = indexed_api_migration_fixture(initial).await;
        let symbol = api_migration_symbol(&graph, "wire_name").await;
        let request = ApiMigrationPlanRequestV1 {
            family_id: "family.protected-overwrite".to_owned(),
            operations: vec![
                ApiMigrationOperationRequestV1::PromotePrimary {
                    operation_id: "promote".to_owned(),
                    depends_on: Vec::new(),
                    symbol: symbol.clone(),
                    expected_definition_digest: api_migration_definition_digest(initial).unwrap(),
                    replacement_definition: replacement.to_owned(),
                },
                ApiMigrationOperationRequestV1::AssertStableValue {
                    operation_id: "protect-wire-name".to_owned(),
                    depends_on: vec!["promote".to_owned()],
                    enclosing_symbol: symbol,
                    category: "wire field".to_owned(),
                    exact_bytes: "\"stable_name\"".to_owned(),
                    occurrence_indexes: vec![0],
                },
            ],
        };

        let plan = crate::api_migration::plan_api_migration(&graph, request)
            .await
            .unwrap();

        assert!(plan.blocked);
        assert!(plan.sites.iter().any(|site| {
            site.operation_id == "protect-wire-name"
                && site.disposition == ApiMigrationSiteDispositionV1::Blocked
        }));
        assert_eq!(
            plan.files[0].expected_content,
            plan.files[0].intended_content
        );
    }

    #[test]
    fn api_migration_apply_requires_the_current_graph_backed_plan() {
        let supplied = ApiMigrationPlanV1 {
            family_id: "family".to_owned(),
            repository_revision: "revision".to_owned(),
            graph_revision: digest(SHA256_A),
            operations: Vec::new(),
            sites: Vec::new(),
            files: Vec::new(),
            blocked: false,
            plan_digest: digest(SHA256_B),
        };
        let mut replanned = supplied.clone();
        assert!(validate_replanned_api_migration(&supplied, &replanned).is_ok());
        replanned.graph_revision = digest(SHA256_B);
        assert!(validate_replanned_api_migration(&supplied, &replanned).is_err());
    }
}
