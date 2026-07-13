use super::model::{
    CompatibilityInventoryV1, EntityDispositionV1, InventoryValidationError, PlatformDispositionV1,
    RouteStatusV1,
};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum InventoryRenderError {
    #[error("invalid compatibility inventory: {0}")]
    Validation(#[source] InventoryValidationError),
    #[error("failed to render compatibility inventory: {0}")]
    Json(#[source] serde_json::Error),
}

/// Serializes only the validated semantic snapshot. Run metadata is a separate
/// envelope and therefore cannot influence these bytes or their digest.
pub fn canonical_semantic_json_bytes(
    inventory: &CompatibilityInventoryV1,
) -> Result<Vec<u8>, InventoryRenderError> {
    inventory
        .validate()
        .map_err(InventoryRenderError::Validation)?;
    serde_json::to_vec(inventory).map_err(InventoryRenderError::Json)
}

pub fn semantic_snapshot_digest(
    inventory: &CompatibilityInventoryV1,
) -> Result<String, InventoryRenderError> {
    let bytes = canonical_semantic_json_bytes(inventory)?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

/// Renders a compact, content-free projection of every semantic entry and
/// source family. The schema and digest bind every omitted canonical field.
pub fn render_compact_markdown(
    inventory: &CompatibilityInventoryV1,
) -> Result<String, InventoryRenderError> {
    let digest = semantic_snapshot_digest(inventory)?;
    let mut output = String::from("# Compatibility inventory\n\n");
    output.push_str(&format!(
        "Schema: `{}`\n\nSemantic snapshot: `{digest}`\n\n",
        markdown_cell(&inventory.schema),
    ));
    output.push_str(&format!(
        "Semantic entries: {}\n\n",
        inventory.entries.len()
    ));
    output.push_str("## Entries\n\n");
    output.push_str("| Stable ID | Status | Parity blocker | Cutover blocker |\n");
    output.push_str("|---|---|---|---|\n");
    for entry in &inventory.entries {
        output.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            markdown_cell(&entry.stable_id),
            route_status(entry.route_status),
            markdown_cell(&entry.gates.parity_gate),
            markdown_cell(&entry.gates.cutover_gate),
        ));
    }

    output.push_str("\n## Counts\n\n");
    render_count_group(
        &mut output,
        "Kind",
        inventory
            .summaries
            .entries_by_kind
            .iter()
            .map(|(key, count)| (key.as_str(), *count)),
    );
    render_count_group(
        &mut output,
        "Route status",
        inventory
            .summaries
            .entries_by_route_status
            .iter()
            .map(|(key, count)| (route_status(*key), *count)),
    );
    render_count_group(
        &mut output,
        "Entity disposition",
        inventory
            .summaries
            .entries_by_entity_disposition
            .iter()
            .map(|(key, count)| (entity_disposition(*key), *count)),
    );
    render_count_group(
        &mut output,
        "Platform disposition",
        inventory
            .summaries
            .entries_by_platform_disposition
            .iter()
            .map(|(key, count)| (platform_disposition(*key), *count)),
    );

    output.push_str("\n## Source-family appendix\n\n");
    output.push_str(
        "| Stable ID | Family | Owner | Entry refs | Paths | Tables | Indexes | Triggers | Sidecars |\n",
    );
    output.push_str("|---|---|---|---|---:|---:|---:|---:|---:|\n");
    for appendix in &inventory.source_family_appendix {
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            markdown_cell(&appendix.stable_id),
            markdown_cell(&appendix.source_family),
            markdown_cell(&appendix.owner),
            markdown_list(&appendix.entry_refs),
            appendix.relative_paths_or_globs.len(),
            appendix.tables.len(),
            appendix.indexes.len(),
            appendix.triggers.len(),
            appendix.sidecars.len(),
        ));
    }

    Ok(output)
}

fn render_count_group<'a>(
    output: &mut String,
    heading: &str,
    values: impl Iterator<Item = (&'a str, u64)>,
) {
    output.push_str(&format!("### {heading}\n\n"));
    output.push_str("| Value | Count |\n|---|---:|\n");
    for (value, count) in values {
        output.push_str(&format!("| {} | {count} |\n", markdown_cell(value)));
    }
    output.push('\n');
}

fn markdown_list(values: &[String]) -> String {
    if values.is_empty() {
        return "-".to_owned();
    }
    values
        .iter()
        .map(|value| markdown_cell(value))
        .collect::<Vec<_>>()
        .join("<br>")
}

fn markdown_cell(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

fn route_status(value: RouteStatusV1) -> &'static str {
    match value {
        RouteStatusV1::V1Only => "v1_only",
        RouteStatusV1::V2Shadow => "v2_shadow",
        RouteStatusV1::ParityProven => "parity_proven",
        RouteStatusV1::V2Default => "v2_default",
        RouteStatusV1::MigrationOnly => "migration_only",
        RouteStatusV1::Retired => "retired",
    }
}

fn entity_disposition(value: EntityDispositionV1) -> &'static str {
    match value {
        EntityDispositionV1::Retained => "retained",
        EntityDispositionV1::Skipped => "skipped",
        EntityDispositionV1::Quarantined => "quarantined",
        EntityDispositionV1::Redacted => "redacted",
        EntityDispositionV1::Deleted => "deleted",
    }
}

fn platform_disposition(value: PlatformDispositionV1) -> &'static str {
    match value {
        PlatformDispositionV1::Supported => "supported",
        PlatformDispositionV1::Alternative => "alternative",
        PlatformDispositionV1::Unavailable => "unavailable",
        PlatformDispositionV1::Untested => "untested",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compatibility_inventory::model::{
        COMPATIBILITY_INVENTORY_SCHEMA_V1, CompatibilityEntryV1, InventoryGatesV1,
        InventoryOwnersV1, InventorySummariesV1, SourceFamilyAppendixEntryV1,
    };
    fn inventory() -> CompatibilityInventoryV1 {
        let entry = CompatibilityEntryV1 {
            stable_id: "store:activity".to_owned(),
            kind: "store".to_owned(),
            canonical_name: "private content omitted from report".to_owned(),
            source_refs: vec!["src/global_db.rs".to_owned()],
            platform: "all".to_owned(),
            route_status: RouteStatusV1::V1Only,
            entity_disposition: EntityDispositionV1::Retained,
            platform_disposition: Some(PlatformDispositionV1::Supported),
            owners: InventoryOwnersV1 {
                v1_owner: "root".to_owned(),
                v2_owner: "tracedecay-store".to_owned(),
            },
            readers: vec!["reader".to_owned()],
            writers: vec!["writer".to_owned()],
            tests: vec!["inventory_is_complete".to_owned()],
            gates: InventoryGatesV1 {
                parity_gate: "PR3-PARITY".to_owned(),
                cutover_gate: "PR37-CUTOVER".to_owned(),
            },
            recovery: "restore archive".to_owned(),
            delete_by_pr: "PR 37".to_owned(),
        };
        let summaries = InventorySummariesV1::from_entries(std::slice::from_ref(&entry));
        CompatibilityInventoryV1 {
            schema: COMPATIBILITY_INVENTORY_SCHEMA_V1.to_owned(),
            entries: vec![entry],
            source_family_appendix: vec![SourceFamilyAppendixEntryV1 {
                stable_id: "source:activity".to_owned(),
                source_family: "activity".to_owned(),
                relative_paths_or_globs: vec![".tracedecay/activity.db".to_owned()],
                tables: vec!["sessions".to_owned()],
                indexes: vec![],
                triggers: vec![],
                sidecars: vec!["wal".to_owned()],
                owner: "root".to_owned(),
                entry_refs: vec!["store:activity".to_owned()],
            }],
            summaries,
        }
    }

    #[test]
    fn canonical_bytes_and_digest_are_deterministic() {
        let inventory = inventory();
        let first = canonical_semantic_json_bytes(&inventory).unwrap();
        let second = canonical_semantic_json_bytes(&inventory).unwrap();
        assert_eq!(first, second);
        assert_eq!(semantic_snapshot_digest(&inventory).unwrap().len(), 71);
        assert_eq!(
            serde_json::from_slice::<CompatibilityInventoryV1>(&first).unwrap(),
            inventory
        );
    }

    #[test]
    fn markdown_projection_is_bound_to_canonical_snapshot() {
        let inventory = inventory();
        let digest = semantic_snapshot_digest(&inventory).unwrap();
        let markdown = render_compact_markdown(&inventory).unwrap();
        for required in [
            COMPATIBILITY_INVENTORY_SCHEMA_V1,
            digest.as_str(),
            "store:activity",
            "v1_only",
            "PR3-PARITY",
            "PR37-CUTOVER",
            "| store | 1 |",
            "source:activity",
            "| source:activity | activity | root | store:activity | 1 | 1 | 0 | 0 | 1 |",
        ] {
            assert!(markdown.contains(required), "missing {required:?}");
        }
        assert!(!markdown.contains("private content omitted from report"));
        assert!(!markdown.contains(".tracedecay/activity.db"));
    }

    #[test]
    fn omitted_fields_cannot_collide_in_markdown_projection() {
        let first = inventory();
        let mut changed_entry = first.clone();
        changed_entry.entries[0].canonical_name = "different private value".to_owned();
        let mut changed_appendix = first.clone();
        changed_appendix.source_family_appendix[0].relative_paths_or_globs =
            vec![".tracedecay/different.db".to_owned()];

        let first_markdown = render_compact_markdown(&first).unwrap();
        let changed_entry_markdown = render_compact_markdown(&changed_entry).unwrap();
        let changed_appendix_markdown = render_compact_markdown(&changed_appendix).unwrap();

        assert_ne!(first_markdown, changed_entry_markdown);
        assert_ne!(first_markdown, changed_appendix_markdown);
        assert!(!changed_entry_markdown.contains("different private value"));
        assert!(!changed_appendix_markdown.contains(".tracedecay/different.db"));
    }

    #[test]
    fn invalid_inventory_is_never_rendered() {
        let mut inventory = inventory();
        inventory.summaries.entries_by_kind.clear();
        assert!(matches!(
            render_compact_markdown(&inventory),
            Err(InventoryRenderError::Validation(_))
        ));
    }
}
