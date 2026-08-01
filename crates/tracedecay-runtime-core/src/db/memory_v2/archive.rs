use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::{FactOwnerV1, ProjectId, SourceStoreId, UtcMicros};
use tracedecay_store::{
    MemoryV2ArchiveFamilyV1, MemoryV2ArchiveRecordV1, MemoryV2ArchiveReferenceV1,
    MemoryV2ArchiveScalarV1, MemoryV2OwnerArchiveV1, MemoryV2OwnerMergePlanV1,
    RetrievalAnchorOwnerV1, authoritative_memory_v2_archive_families, plan_memory_v2_owner_merge,
};

use crate::db::engine::{Executor, Value, params, params_from_iter};
use crate::errors::Result;

use super::{
    db_error, db_message, json_text, mark_memory_v2_compatibility_bank_dirty_in_transaction,
};

const OPERATION: &str = "memory_v2_owner_archive";
const LEGACY_MEMORY_SOURCE_STORE: &str = "legacy-memory-v1";
const COMPATIBILITY_BANKS: [&str; 7] = [
    "all",
    "general",
    "user_pref",
    "project",
    "tool",
    "decision",
    "code_area",
];

#[derive(Clone, Copy)]
pub enum MemoryV2ArchiveDatabase {
    Main,
    Source,
}

impl MemoryV2ArchiveDatabase {
    fn prefix(self) -> &'static str {
        match self {
            Self::Main => "",
            Self::Source => "source.",
        }
    }

    fn schema_name(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Source => "source",
        }
    }
}

#[derive(Clone, Copy)]
enum OwnerFilter {
    Scope,
    RetrievalAnchor,
    RetrievalAnchorChild(&'static str),
    EvidenceOwnerDigest,
    EvidenceParent(&'static str, &'static str),
}

struct TableSpec {
    family: MemoryV2ArchiveFamilyV1,
    table: &'static str,
    columns: &'static [&'static str],
    key_columns: &'static [&'static str],
    owner_filter: OwnerFilter,
}

struct PhysicalForeignKey {
    target_family: MemoryV2ArchiveFamilyV1,
    target_identity_columns: Vec<String>,
    columns: Vec<(String, String)>,
}

pub async fn list_memory_v2_archive_owners(
    conn: &(impl Executor + Sync),
    database: MemoryV2ArchiveDatabase,
) -> Result<Vec<FactOwnerV1>> {
    let prefix = database.prefix();
    let sql = format!(
        "SELECT owner_kind, project_id FROM {prefix}memory_v2_facts
         UNION SELECT owner_kind, project_id FROM {prefix}memory_v2_assertions
         UNION SELECT owner_kind, project_id FROM {prefix}memory_v2_evidence
         UNION SELECT owner_kind, project_id FROM {prefix}memory_v2_current_facts
         UNION SELECT owner_kind, project_id FROM {prefix}memory_v2_legacy_map
         UNION SELECT owner_kind, project_id
               FROM {prefix}memory_v2_legacy_feedback_event_map
         UNION SELECT owner_kind, project_id FROM {prefix}memory_v2_feedback_history
         UNION SELECT owner_kind, project_id FROM {prefix}memory_v2_fact_relations
         UNION SELECT owner_kind, project_id FROM {prefix}memory_v2_proposals
         UNION SELECT owner_kind, project_id FROM {prefix}memory_v2_proposal_transitions
         UNION SELECT owner_kind, project_id FROM {prefix}memory_v2_proposal_current
         UNION SELECT owner_kind, project_id FROM {prefix}memory_v2_legacy_quarantine
         UNION SELECT owner_kind, project_id
               FROM {prefix}memory_v2_compatibility_operation_receipts
         ORDER BY owner_kind, project_id"
    );
    let mut rows = conn
        .query(&sql, ())
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut owners = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let kind: String = row.get(0).map_err(|error| db_error(OPERATION, error))?;
        let project_id: String = row.get(1).map_err(|error| db_error(OPERATION, error))?;
        owners.push(owner_from_key(&kind, &project_id)?);
    }
    let sql = format!(
        "SELECT DISTINCT anchor.owner_json
         FROM {prefix}retrieval_anchors AS anchor
         WHERE anchor.anchor_id IN (
             SELECT source_anchor_id FROM {prefix}evidence_source_occurrences
             UNION SELECT anchor_id FROM {prefix}evidence_spans
             UNION SELECT anchor_id FROM {prefix}evidence_retriever_contributions
             UNION SELECT anchor_id FROM {prefix}evidence_derived_anchors
         )
         ORDER BY anchor.owner_json"
    );
    let mut rows = conn
        .query(&sql, ())
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let owner_json: String = row.get(0).map_err(|error| db_error(OPERATION, error))?;
        let owner: RetrievalAnchorOwnerV1 = serde_json::from_str(&owner_json)
            .map_err(|error| db_message(OPERATION, error.to_string()))?;
        let owner = match owner {
            RetrievalAnchorOwnerV1::V2(owner) => owner,
            RetrievalAnchorOwnerV1::V3(owner) => match owner.project_id() {
                Some(project_id) => FactOwnerV1::Project {
                    project_id: project_id.clone(),
                },
                None => FactOwnerV1::Profile,
            },
        };
        if !owners.contains(&owner) {
            owners.push(owner);
        }
    }
    owners.sort_by_key(|owner| serde_json::to_string(owner).unwrap_or_default());
    Ok(owners)
}

pub async fn export_memory_v2_owner_archive(
    conn: &(impl Executor + Sync),
    database: MemoryV2ArchiveDatabase,
    owner: &FactOwnerV1,
) -> Result<MemoryV2OwnerArchiveV1> {
    validate_table_specs()?;
    validate_physical_schema(conn, database).await?;
    owner
        .validate()
        .map_err(|error| db_message(OPERATION, error.to_string()))?;
    let (owner_kind, project_id) = owner_key(owner);
    let owner_json = json_text(owner)?;
    let mut records = Vec::new();
    for spec in table_specs() {
        records.extend(
            export_table(conn, database, spec, owner_kind, &project_id, &owner_json).await?,
        );
    }
    MemoryV2OwnerArchiveV1::new(
        owner.clone(),
        authoritative_memory_v2_archive_families(),
        records,
    )
    .map_err(|error| db_message(OPERATION, format!("{error}: {error:?}")))
}

pub async fn plan_memory_v2_owner_archive_import(
    conn: &(impl Executor + Sync),
    archive: &MemoryV2OwnerArchiveV1,
) -> Result<MemoryV2OwnerMergePlanV1> {
    let target =
        export_memory_v2_owner_archive(conn, MemoryV2ArchiveDatabase::Main, archive.owner())
            .await?;
    plan_memory_v2_owner_merge(archive, &target)
        .map_err(|error| db_message(OPERATION, error.to_string()))
}

pub async fn import_memory_v2_owner_archive(
    conn: &(impl Executor + Sync),
    archive: &MemoryV2OwnerArchiveV1,
    plan: &MemoryV2OwnerMergePlanV1,
) -> Result<()> {
    validate_table_specs()?;
    validate_physical_schema(conn, MemoryV2ArchiveDatabase::Main).await?;
    if plan.owner() != archive.owner()
        || plan.source_digest()
            != &archive
                .digest()
                .map_err(|error| db_message(OPERATION, error.to_string()))?
    {
        return Err(db_message(
            OPERATION,
            "archive import plan does not bind the supplied archive",
        ));
    }
    if !plan.can_apply() {
        return Err(db_message(
            OPERATION,
            format!(
                "archive import has {} incompatible stable identities; first conflict: {:?}",
                plan.conflicts().len(),
                plan.conflicts().first()
            ),
        ));
    }
    let specs: BTreeMap<_, _> = table_specs()
        .iter()
        .map(|spec| (spec.family, spec))
        .collect();
    for record in plan.inserts() {
        let spec = specs
            .get(&record.family())
            .ok_or_else(|| db_message(OPERATION, "archive record has no physical table adapter"))?;
        let source_order = specs
            .keys()
            .position(|family| family == &record.family())
            .ok_or_else(|| db_message(OPERATION, "archive family has no insertion order"))?;
        for reference in record.references() {
            let target_order = specs
                .keys()
                .position(|family| family == &reference.family())
                .ok_or_else(|| db_message(OPERATION, "archive reference family is unsupported"))?;
            if target_order > source_order {
                return Err(db_message(
                    OPERATION,
                    format!(
                        "archive family {:?} precedes its {:?} dependency",
                        record.family(),
                        reference.family()
                    ),
                ));
            }
        }
        insert_record(conn, spec, record).await?;
    }
    for record in plan.updates() {
        let spec = specs
            .get(&record.family())
            .ok_or_else(|| db_message(OPERATION, "archive update has no physical adapter"))?;
        update_projection_record(conn, spec, record).await?;
    }
    let verified =
        export_memory_v2_owner_archive(conn, MemoryV2ArchiveDatabase::Main, archive.owner())
            .await?;
    let verification = plan_memory_v2_owner_merge(archive, &verified)
        .map_err(|error| db_message(OPERATION, error.to_string()))?;
    if !verification.can_apply()
        || !verification.inserts().is_empty()
        || !verification.updates().is_empty()
    {
        return Err(db_message(
            OPERATION,
            format!(
                "archive import readback did not preserve the complete source closure: inserts={:?}, updates={:?}, conflicts={:?}",
                verification.inserts(),
                verification.updates(),
                verification.conflicts(),
            ),
        ));
    }
    if plan.inserts().iter().any(record_changes_fact_projection)
        || plan.updates().iter().any(record_changes_fact_projection)
    {
        mark_imported_owner_banks_dirty(conn, archive).await?;
    }
    Ok(())
}

fn record_changes_fact_projection(record: &MemoryV2ArchiveRecordV1) -> bool {
    matches!(
        record.family(),
        MemoryV2ArchiveFamilyV1::Fact | MemoryV2ArchiveFamilyV1::CurrentFact
    )
}

async fn mark_imported_owner_banks_dirty(
    conn: &(impl Executor + Sync),
    archive: &MemoryV2OwnerArchiveV1,
) -> Result<()> {
    let updated_at = archive
        .records()
        .iter()
        .filter(|record| record.family() == MemoryV2ArchiveFamilyV1::Fact)
        .filter_map(|record| match record.fields().get("created_at") {
            Some(MemoryV2ArchiveScalarV1::Integer(value)) => Some(*value),
            _ => None,
        })
        .max()
        .ok_or_else(|| {
            db_message(
                OPERATION,
                "fact projection changed without an authoritative fact timestamp",
            )
        })?;
    let source_store_id = SourceStoreId::new(LEGACY_MEMORY_SOURCE_STORE)
        .map_err(|error| db_message(OPERATION, error.to_string()))?;
    for bank_name in COMPATIBILITY_BANKS {
        mark_memory_v2_compatibility_bank_dirty_in_transaction(
            conn,
            archive.owner(),
            &source_store_id,
            bank_name,
            UtcMicros(updated_at),
        )
        .await?;
    }
    Ok(())
}

async fn export_table(
    conn: &(impl Executor + Sync),
    database: MemoryV2ArchiveDatabase,
    spec: &TableSpec,
    owner_kind: &str,
    project_id: &str,
    owner_json: &str,
) -> Result<Vec<MemoryV2ArchiveRecordV1>> {
    let physical_foreign_keys = physical_foreign_keys(conn, database, spec).await?;
    let columns = spec
        .columns
        .iter()
        .map(|column| archive_column_expression(database, spec.family, column))
        .collect::<Vec<_>>()
        .join(", ");
    let predicate = owner_predicate(database, spec.owner_filter);
    let visibility = archive_visibility_predicate(database, spec.family);
    let sql = format!(
        "SELECT {columns} FROM {}{} AS archive_row
         WHERE ({predicate}) AND ({visibility}) ORDER BY {}",
        database.prefix(),
        spec.table,
        spec.key_columns
            .iter()
            .map(|column| format!("archive_row.{column}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut rows = conn
        .query(&sql, params![owner_kind, project_id, owner_json])
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut records = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let mut values = BTreeMap::new();
        for (index, column) in spec.columns.iter().enumerate() {
            let index = i32::try_from(index)
                .map_err(|_| db_message(OPERATION, "archive column index overflow"))?;
            let value: Value = row.get(index).map_err(|error| db_error(OPERATION, error))?;
            values.insert((*column).to_owned(), scalar_from_value(value));
        }
        let mut key = BTreeMap::new();
        let mut fields = values.clone();
        for column in spec.key_columns {
            let value = fields.remove(*column).ok_or_else(|| {
                db_message(
                    OPERATION,
                    format!("archive key column `{column}` is absent"),
                )
            })?;
            key.insert((*column).to_owned(), value);
        }
        let references = references_for(spec.family, &values)?;
        validate_physical_foreign_key_references(
            spec,
            &values,
            &references,
            &physical_foreign_keys,
        )?;
        records.push(
            MemoryV2ArchiveRecordV1::new(spec.family, key, fields, references)
                .map_err(|error| db_message(OPERATION, error.to_string()))?,
        );
    }
    Ok(records)
}

fn archive_column_expression(
    database: MemoryV2ArchiveDatabase,
    family: MemoryV2ArchiveFamilyV1,
    column: &str,
) -> String {
    if family == MemoryV2ArchiveFamilyV1::CurrentFact {
        return match column {
            "projection_state" => "'rebuilding'".to_owned(),
            "vector_watermark_json" => "NULL".to_owned(),
            _ => format!("archive_row.{column}"),
        };
    }
    if family != MemoryV2ArchiveFamilyV1::FeedbackHistory
        || !matches!(column, "source" | "note" | "details_availability")
    {
        return format!("archive_row.{column}");
    }
    let prefix = database.prefix();
    let terminal = format!(
        "EXISTS (
            SELECT 1 FROM {prefix}memory_v2_current_facts AS current
            WHERE current.fact_id=archive_row.fact_id
              AND current.owner_kind=archive_row.owner_kind
              AND current.project_id=archive_row.project_id
              AND current.payload_access IN ('expired', 'redacted', 'deleted')
        )"
    );
    if column == "details_availability" {
        format!(
            "CASE WHEN {terminal} THEN 'legacy_redacted'
                  ELSE archive_row.details_availability END"
        )
    } else {
        format!("CASE WHEN {terminal} THEN NULL ELSE archive_row.{column} END")
    }
}

fn archive_visibility_predicate(
    database: MemoryV2ArchiveDatabase,
    family: MemoryV2ArchiveFamilyV1,
) -> String {
    if !matches!(
        family,
        MemoryV2ArchiveFamilyV1::AssertionPayload | MemoryV2ArchiveFamilyV1::AssertionVector
    ) {
        return "1=1".to_owned();
    }
    let prefix = database.prefix();
    format!(
        "NOT EXISTS (
            SELECT 1 FROM {prefix}memory_v2_current_facts AS current
            WHERE current.fact_id=archive_row.fact_id
              AND current.owner_kind=archive_row.owner_kind
              AND current.project_id=archive_row.project_id
              AND current.payload_access IN ('expired', 'redacted', 'deleted')
        )"
    )
}

async fn physical_foreign_keys(
    conn: &(impl Executor + Sync),
    database: MemoryV2ArchiveDatabase,
    spec: &TableSpec,
) -> Result<Vec<PhysicalForeignKey>> {
    type ForeignKeyColumn = (i64, String, String);
    type ForeignKeyGroup = (String, Vec<ForeignKeyColumn>);

    let sql = format!(
        "PRAGMA {}.foreign_key_list('{}')",
        database.schema_name(),
        spec.table
    );
    let mut rows = conn
        .query(&sql, ())
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut grouped: BTreeMap<i64, ForeignKeyGroup> = BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let id: i64 = row.get(0).map_err(|error| db_error(OPERATION, error))?;
        let sequence: i64 = row.get(1).map_err(|error| db_error(OPERATION, error))?;
        let target_table: String = row.get(2).map_err(|error| db_error(OPERATION, error))?;
        let source_column: String = row.get(3).map_err(|error| db_error(OPERATION, error))?;
        let target_column: String = row.get(4).map_err(|error| db_error(OPERATION, error))?;
        let entry = grouped
            .entry(id)
            .or_insert_with(|| (target_table.clone(), Vec::new()));
        if entry.0 != target_table {
            return Err(db_message(
                OPERATION,
                "foreign key table changed within one id",
            ));
        }
        entry.1.push((sequence, source_column, target_column));
    }
    grouped
        .into_values()
        .map(|(target_table, mut columns)| {
            columns.sort_by_key(|(sequence, _, _)| *sequence);
            let target_spec = table_specs()
                .iter()
                .find(|target| target.table == target_table)
                .ok_or_else(|| {
                    db_message(
                        OPERATION,
                        format!(
                            "archive {:?} references non-authoritative table `{target_table}`",
                            spec.family
                        ),
                    )
                })?;
            let target_family = target_spec.family;
            let source_order = table_specs()
                .iter()
                .position(|candidate| candidate.family == spec.family)
                .ok_or_else(|| db_message(OPERATION, "source family has no insertion order"))?;
            let target_order = table_specs()
                .iter()
                .position(|candidate| candidate.family == target_family)
                .ok_or_else(|| db_message(OPERATION, "target family has no insertion order"))?;
            if target_order > source_order {
                return Err(db_message(
                    OPERATION,
                    format!(
                        "archive {:?} is ordered before its physical {:?} dependency",
                        spec.family, target_family
                    ),
                ));
            }
            Ok(PhysicalForeignKey {
                target_family,
                target_identity_columns: target_spec
                    .key_columns
                    .iter()
                    .map(|column| (*column).to_owned())
                    .collect(),
                columns: columns
                    .into_iter()
                    .map(|(_, source, target)| (source, target))
                    .collect(),
            })
        })
        .collect()
}

fn validate_physical_foreign_key_references(
    spec: &TableSpec,
    values: &BTreeMap<String, MemoryV2ArchiveScalarV1>,
    references: &[MemoryV2ArchiveReferenceV1],
    physical_foreign_keys: &[PhysicalForeignKey],
) -> Result<()> {
    for foreign_key in physical_foreign_keys {
        let mut target_key = BTreeMap::new();
        let mut nulls = 0;
        for (source, target) in &foreign_key.columns {
            let value = values.get(source).ok_or_else(|| {
                db_message(
                    OPERATION,
                    format!(
                        "archive {:?} omits foreign-key column `{source}`",
                        spec.family
                    ),
                )
            })?;
            if matches!(value, MemoryV2ArchiveScalarV1::Null) {
                nulls += 1;
            }
            target_key.insert(target.clone(), value.clone());
        }
        // SQLite considers a composite foreign key satisfied when any child
        // column is NULL. Optional references therefore have no target until
        // every component is present.
        if nulls != 0 {
            continue;
        }
        let target_identity = target_key
            .into_iter()
            .filter(|(column, _)| foreign_key.target_identity_columns.contains(column))
            .collect::<BTreeMap<_, _>>();
        if target_identity.len() != foreign_key.target_identity_columns.len()
            || !references.iter().any(|reference| {
                reference.family() == foreign_key.target_family
                    && reference.key() == &target_identity
            })
        {
            return Err(db_message(
                OPERATION,
                format!(
                    "archive {:?} does not encode its complete physical foreign key to {:?}",
                    spec.family, foreign_key.target_family
                ),
            ));
        }
    }
    Ok(())
}

async fn insert_record(
    conn: &(impl Executor + Sync),
    spec: &TableSpec,
    record: &MemoryV2ArchiveRecordV1,
) -> Result<()> {
    validate_record_fields(spec, record)?;
    let placeholders = (1..=spec.columns.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {}({}) VALUES({placeholders})",
        spec.table,
        spec.columns.join(", ")
    );
    let values = spec
        .columns
        .iter()
        .map(|column| {
            record
                .key()
                .get(*column)
                .or_else(|| record.fields().get(*column))
                .cloned()
                .map(value_from_scalar)
                .ok_or_else(|| {
                    db_message(
                        OPERATION,
                        format!("archive record omits physical column `{column}`"),
                    )
                })
        })
        .collect::<Result<Vec<_>>>()?;
    conn.execute(&sql, params_from_iter(values))
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

async fn update_projection_record(
    conn: &(impl Executor + Sync),
    spec: &TableSpec,
    record: &MemoryV2ArchiveRecordV1,
) -> Result<()> {
    if record.family() != MemoryV2ArchiveFamilyV1::CurrentFact {
        return Err(db_message(
            OPERATION,
            "only the derived current-fact projection may be reconciled",
        ));
    }
    validate_record_fields(spec, record)?;
    let field_columns = spec
        .columns
        .iter()
        .filter(|column| !spec.key_columns.contains(column))
        .copied()
        .collect::<Vec<_>>();
    let assignments = field_columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("{column}=?{}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let key_offset = field_columns.len();
    let predicate = spec
        .key_columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("{column}=?{}", key_offset + index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let values = field_columns
        .iter()
        .map(|column| {
            record
                .fields()
                .get(*column)
                .cloned()
                .map(value_from_scalar)
                .ok_or_else(|| db_message(OPERATION, format!("projection update omits `{column}`")))
        })
        .chain(spec.key_columns.iter().map(|column| {
            record
                .key()
                .get(*column)
                .cloned()
                .map(value_from_scalar)
                .ok_or_else(|| {
                    db_message(OPERATION, format!("projection update omits key `{column}`"))
                })
        }))
        .collect::<Result<Vec<_>>>()?;
    conn.execute(
        &format!("UPDATE {} SET {assignments} WHERE {predicate}", spec.table),
        params_from_iter(values),
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

fn validate_record_fields(spec: &TableSpec, record: &MemoryV2ArchiveRecordV1) -> Result<()> {
    let expected: BTreeSet<_> = spec.columns.iter().copied().collect();
    let actual: BTreeSet<_> = record
        .key()
        .keys()
        .chain(record.fields().keys())
        .map(String::as_str)
        .collect();
    if actual != expected {
        return Err(db_message(
            OPERATION,
            format!(
                "archive {:?} physical fields do not match its schema adapter",
                record.family()
            ),
        ));
    }
    Ok(())
}

fn owner_predicate(database: MemoryV2ArchiveDatabase, filter: OwnerFilter) -> String {
    let prefix = database.prefix();
    let anchor_owner = "(
        owner_json=?3
        OR (
            json_extract(owner_json, '$.kind')=?1
            AND (
                (?1='profile' AND ?2='')
                OR json_extract(owner_json, '$.project_id')=?2
            )
        )
    )";
    let evidence_anchor_owner = "(
        anchor.owner_json=?3
        OR (
            json_extract(anchor.owner_json, '$.kind')=?1
            AND (
                (?1='profile' AND ?2='')
                OR json_extract(anchor.owner_json, '$.project_id')=?2
            )
        )
    )";
    let selected_anchors = format!(
        "SELECT anchor_id FROM {prefix}retrieval_anchors WHERE {anchor_owner}
         UNION
         SELECT anchor.anchor_id
         FROM {prefix}retrieval_anchors AS anchor
         WHERE anchor.owner_json IN (
             SELECT selected.owner_json
             FROM {prefix}retrieval_anchors AS selected
             JOIN {prefix}memory_v2_evidence AS evidence
               ON evidence.anchor_id=selected.anchor_id
             WHERE evidence.owner_kind=?1 AND evidence.project_id=?2
         )"
    );
    let evidence_owners = format!(
        "SELECT occurrence.owner_digest
         FROM {prefix}evidence_source_occurrences AS occurrence
         JOIN {prefix}retrieval_anchors AS anchor
           ON anchor.anchor_id=occurrence.source_anchor_id
         WHERE {evidence_anchor_owner}
         UNION
         SELECT span.owner_digest
         FROM {prefix}evidence_spans AS span
         JOIN {prefix}retrieval_anchors AS anchor
           ON anchor.anchor_id=span.anchor_id
         WHERE {evidence_anchor_owner}
         UNION
         SELECT contribution.owner_digest
         FROM {prefix}evidence_retriever_contributions AS contribution
         JOIN {prefix}retrieval_anchors AS anchor
           ON anchor.anchor_id=contribution.anchor_id
         WHERE {evidence_anchor_owner}
         UNION
         SELECT derived.owner_digest
         FROM {prefix}evidence_derived_anchors AS derived
         JOIN {prefix}retrieval_anchors AS anchor
           ON anchor.anchor_id=derived.anchor_id
         WHERE anchor.owner_json=?3
         UNION
         SELECT derived.owner_digest
         FROM {prefix}evidence_derived_anchors AS derived
         JOIN {prefix}memory_v2_evidence AS evidence
           ON evidence.anchor_id=derived.anchor_id
         WHERE evidence.owner_kind=?1 AND evidence.project_id=?2"
    );
    match filter {
        OwnerFilter::Scope => "owner_kind=?1 AND project_id=?2 AND ?3=?3".to_owned(),
        OwnerFilter::RetrievalAnchor => format!("anchor_id IN ({selected_anchors})"),
        OwnerFilter::RetrievalAnchorChild(column) => {
            format!("{column} IN ({selected_anchors})")
        }
        OwnerFilter::EvidenceOwnerDigest => {
            format!("owner_digest IN ({evidence_owners}) AND ?3=?3")
        }
        OwnerFilter::EvidenceParent(parent, owner_column) => format!(
            "{parent} IN (
                SELECT {parent} FROM {prefix}{owner_column}
                WHERE owner_digest IN ({evidence_owners})
             ) AND ?3=?3"
        ),
    }
}

fn owner_key(owner: &FactOwnerV1) -> (&'static str, String) {
    match owner {
        FactOwnerV1::Profile => ("profile", String::new()),
        FactOwnerV1::Project { project_id } => ("project", project_id.as_str().to_owned()),
    }
}

fn owner_from_key(kind: &str, project_id: &str) -> Result<FactOwnerV1> {
    match (kind, project_id) {
        ("profile", "") => Ok(FactOwnerV1::Profile),
        ("project", project_id) if !project_id.is_empty() => ProjectId::new(project_id.to_owned())
            .map(|project_id| FactOwnerV1::Project { project_id })
            .map_err(|error| db_message(OPERATION, error.to_string())),
        _ => Err(db_message(
            OPERATION,
            "archive source contains an invalid owner scope",
        )),
    }
}

fn scalar_from_value(value: Value) -> MemoryV2ArchiveScalarV1 {
    match value {
        Value::Null => MemoryV2ArchiveScalarV1::Null,
        Value::Integer(value) => MemoryV2ArchiveScalarV1::Integer(value),
        Value::Real(value) => MemoryV2ArchiveScalarV1::RealBits(value.to_bits()),
        Value::Text(value) => MemoryV2ArchiveScalarV1::Text(value),
        Value::Blob(value) => MemoryV2ArchiveScalarV1::Blob(value),
    }
}

fn value_from_scalar(value: MemoryV2ArchiveScalarV1) -> Value {
    match value {
        MemoryV2ArchiveScalarV1::Null => Value::Null,
        MemoryV2ArchiveScalarV1::Integer(value) => Value::Integer(value),
        MemoryV2ArchiveScalarV1::RealBits(value) => Value::Real(f64::from_bits(value)),
        MemoryV2ArchiveScalarV1::Text(value) => Value::Text(value),
        MemoryV2ArchiveScalarV1::Blob(value) => Value::Blob(value),
    }
}

fn references_for(
    family: MemoryV2ArchiveFamilyV1,
    values: &BTreeMap<String, MemoryV2ArchiveScalarV1>,
) -> Result<Vec<MemoryV2ArchiveReferenceV1>> {
    use MemoryV2ArchiveFamilyV1 as Family;

    let mut references = Vec::new();
    match family {
        Family::RetrievalAnchor
        | Family::EvidenceOccurrenceSet
        | Family::Fact
        | Family::LegacyQuarantine
        | Family::Proposal => {}
        Family::RetrievalAnchorAlias => {
            references.push(reference(
                Family::RetrievalAnchor,
                values,
                &[("anchor_id", "anchor_id")],
            )?);
        }
        Family::RetrievalAnchorDisposition => {
            references.push(reference(
                Family::RetrievalAnchor,
                values,
                &[("anchor_id", "anchor_id")],
            )?);
            push_optional_reference(
                &mut references,
                Family::RetrievalAnchor,
                values,
                &[("superseded_by", "anchor_id")],
            )?;
        }
        Family::RetrievalAnchorReverseLineage => {
            references.push(reference(
                Family::RetrievalAnchor,
                values,
                &[("source_anchor_id", "anchor_id")],
            )?);
        }
        Family::RetrievalAnchorDerivativeTombstone => {
            references.push(reference(
                Family::RetrievalAnchorReverseLineage,
                values,
                &[
                    ("source_anchor_id", "source_anchor_id"),
                    ("owner_json", "owner_json"),
                    ("derivative_kind", "derivative_kind"),
                    ("derivative_id", "derivative_id"),
                ],
            )?);
        }
        Family::EvidenceSourceOccurrence => {}
        Family::EvidenceOccurrenceSetMember => {
            references.push(reference(
                Family::EvidenceOccurrenceSet,
                values,
                &[("occurrence_set_id", "occurrence_set_id")],
            )?);
            references.push(reference(
                Family::EvidenceSourceOccurrence,
                values,
                &[("occurrence_id", "occurrence_id")],
            )?);
        }
        Family::EvidenceSpan => {
            references.push(reference(
                Family::EvidenceOccurrenceSet,
                values,
                &[("occurrence_set_id", "occurrence_set_id")],
            )?);
        }
        Family::EvidenceSpanMember => {
            references.push(reference(
                Family::EvidenceSpan,
                values,
                &[("span_id", "span_id")],
            )?);
            references.push(reference(
                Family::EvidenceSourceOccurrence,
                values,
                &[("occurrence_id", "occurrence_id")],
            )?);
        }
        Family::EvidenceSpanProjectionReceipt => {
            references.push(reference(
                Family::EvidenceSpan,
                values,
                &[("span_id", "span_id")],
            )?);
        }
        Family::EvidenceRetrieverContribution => {
            references.push(reference(
                Family::EvidenceSpan,
                values,
                &[("span_id", "span_id")],
            )?);
        }
        Family::EvidenceDerivedAnchor => {
            let target_family = match text_value(values, "target_kind")? {
                "source_occurrence" => Family::EvidenceSourceOccurrence,
                "evidence_span" => Family::EvidenceSpan,
                "retriever_contribution" => Family::EvidenceRetrieverContribution,
                _ => {
                    return Err(db_message(
                        OPERATION,
                        "derived evidence anchor has an unknown target kind",
                    ));
                }
            };
            let target_key = match target_family {
                Family::EvidenceSourceOccurrence => "occurrence_id",
                Family::EvidenceSpan => "span_id",
                Family::EvidenceRetrieverContribution => "contribution_id",
                _ => unreachable!(),
            };
            references.push(reference(
                target_family,
                values,
                &[("target_id", target_key)],
            )?);
        }
        Family::EvidenceAssemblyReceipt => {
            for (target, column, target_column) in [
                (
                    Family::EvidenceOccurrenceSet,
                    "occurrence_set_id",
                    "occurrence_set_id",
                ),
                (Family::EvidenceSpan, "span_id", "span_id"),
                (
                    Family::EvidenceRetrieverContribution,
                    "contribution_id",
                    "contribution_id",
                ),
                (
                    Family::EvidenceSpanProjectionReceipt,
                    "projection_receipt_id",
                    "projection_receipt_id",
                ),
            ] {
                references.push(reference(target, values, &[(column, target_column)])?);
            }
        }
        Family::Assertion => references.push(fact_reference(values, "fact_id")?),
        Family::AssertionSupersession => {
            references.push(assertion_reference(values, "assertion_id")?);
            references.push(assertion_reference(values, "superseded_assertion_id")?);
        }
        Family::AssertionPayload => {
            references.push(assertion_reference(values, "assertion_id")?);
        }
        Family::AssertionVector => {
            references.push(reference(
                Family::AssertionPayload,
                values,
                &[
                    ("assertion_id", "assertion_id"),
                    ("fact_id", "fact_id"),
                    ("owner_kind", "owner_kind"),
                    ("project_id", "project_id"),
                ],
            )?);
        }
        Family::FactEvidence => {
            references.push(fact_reference(values, "fact_id")?);
            references.push(reference(
                Family::RetrievalAnchor,
                values,
                &[("anchor_id", "anchor_id")],
            )?);
        }
        Family::AssertionEvidence => {
            references.push(assertion_reference(values, "assertion_id")?);
            references.push(reference(
                Family::FactEvidence,
                values,
                &[
                    ("evidence_id", "evidence_id"),
                    ("fact_id", "fact_id"),
                    ("owner_kind", "owner_kind"),
                    ("project_id", "project_id"),
                ],
            )?);
        }
        Family::LineageEvent => references.push(fact_reference(values, "fact_id")?),
        Family::CurrentFact => {
            references.push(fact_reference(values, "fact_id")?);
            push_optional_reference(
                &mut references,
                Family::Assertion,
                values,
                &[
                    ("active_assertion_id", "assertion_id"),
                    ("fact_id", "fact_id"),
                    ("owner_kind", "owner_kind"),
                    ("project_id", "project_id"),
                ],
            )?;
            references.push(event_reference(values, "last_event_id")?);
        }
        Family::LegacyFactMap => references.push(fact_reference(values, "fact_id")?),
        Family::CompatibilityOperationReceipt => {
            push_optional_reference(
                &mut references,
                Family::Fact,
                values,
                &[
                    ("fact_id", "fact_id"),
                    ("owner_kind", "owner_kind"),
                    ("project_id", "project_id"),
                ],
            )?;
            push_optional_reference(
                &mut references,
                Family::LineageEvent,
                values,
                &[
                    ("event_id", "event_id"),
                    ("fact_id", "fact_id"),
                    ("owner_kind", "owner_kind"),
                    ("project_id", "project_id"),
                ],
            )?;
        }
        Family::LegacyFeedbackEventMap | Family::FeedbackHistory => {
            references.push(fact_reference(values, "fact_id")?);
            references.push(event_reference(values, "event_id")?);
        }
        Family::FactRelation => {
            references.push(fact_reference(values, "source_fact_id")?);
            references.push(fact_reference(values, "target_fact_id")?);
        }
        Family::ProposalTransition => {
            references.push(proposal_reference(values)?);
            push_optional_reference(
                &mut references,
                Family::Fact,
                values,
                &[
                    ("promoted_fact_id", "fact_id"),
                    ("owner_kind", "owner_kind"),
                    ("project_id", "project_id"),
                ],
            )?;
            push_optional_reference(
                &mut references,
                Family::Assertion,
                values,
                &[
                    ("promoted_assertion_id", "assertion_id"),
                    ("promoted_fact_id", "fact_id"),
                    ("owner_kind", "owner_kind"),
                    ("project_id", "project_id"),
                ],
            )?;
            push_optional_reference(
                &mut references,
                Family::LineageEvent,
                values,
                &[
                    ("promoted_event_id", "event_id"),
                    ("promoted_fact_id", "fact_id"),
                    ("owner_kind", "owner_kind"),
                    ("project_id", "project_id"),
                ],
            )?;
        }
        Family::ProposalCurrent => {
            references.push(proposal_reference(values)?);
            references.push(reference(
                Family::ProposalTransition,
                values,
                &[
                    ("last_transition_id", "transition_id"),
                    ("proposal_id", "proposal_id"),
                    ("owner_kind", "owner_kind"),
                    ("project_id", "project_id"),
                ],
            )?);
        }
        Family::LegacyProposalMap => references.push(proposal_reference(values)?),
    }
    Ok(references)
}

fn fact_reference(
    values: &BTreeMap<String, MemoryV2ArchiveScalarV1>,
    fact_column: &str,
) -> Result<MemoryV2ArchiveReferenceV1> {
    reference(
        MemoryV2ArchiveFamilyV1::Fact,
        values,
        &[
            (fact_column, "fact_id"),
            ("owner_kind", "owner_kind"),
            ("project_id", "project_id"),
        ],
    )
}

fn assertion_reference(
    values: &BTreeMap<String, MemoryV2ArchiveScalarV1>,
    assertion_column: &str,
) -> Result<MemoryV2ArchiveReferenceV1> {
    reference(
        MemoryV2ArchiveFamilyV1::Assertion,
        values,
        &[
            (assertion_column, "assertion_id"),
            ("fact_id", "fact_id"),
            ("owner_kind", "owner_kind"),
            ("project_id", "project_id"),
        ],
    )
}

fn event_reference(
    values: &BTreeMap<String, MemoryV2ArchiveScalarV1>,
    event_column: &str,
) -> Result<MemoryV2ArchiveReferenceV1> {
    reference(
        MemoryV2ArchiveFamilyV1::LineageEvent,
        values,
        &[
            (event_column, "event_id"),
            ("fact_id", "fact_id"),
            ("owner_kind", "owner_kind"),
            ("project_id", "project_id"),
        ],
    )
}

fn proposal_reference(
    values: &BTreeMap<String, MemoryV2ArchiveScalarV1>,
) -> Result<MemoryV2ArchiveReferenceV1> {
    reference(
        MemoryV2ArchiveFamilyV1::Proposal,
        values,
        &[
            ("proposal_id", "proposal_id"),
            ("owner_kind", "owner_kind"),
            ("project_id", "project_id"),
        ],
    )
}

fn reference(
    family: MemoryV2ArchiveFamilyV1,
    values: &BTreeMap<String, MemoryV2ArchiveScalarV1>,
    columns: &[(&str, &str)],
) -> Result<MemoryV2ArchiveReferenceV1> {
    let key = columns
        .iter()
        .map(|(source, target)| {
            values
                .get(*source)
                .cloned()
                .map(|value| ((*target).to_owned(), value))
                .ok_or_else(|| {
                    db_message(
                        OPERATION,
                        format!("archive reference column `{source}` is absent"),
                    )
                })
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    MemoryV2ArchiveReferenceV1::new(family, key)
        .map_err(|error| db_message(OPERATION, error.to_string()))
}

fn push_optional_reference(
    references: &mut Vec<MemoryV2ArchiveReferenceV1>,
    family: MemoryV2ArchiveFamilyV1,
    values: &BTreeMap<String, MemoryV2ArchiveScalarV1>,
    columns: &[(&str, &str)],
) -> Result<()> {
    let Some((source, _)) = columns.first() else {
        return Ok(());
    };
    if !matches!(values.get(*source), Some(MemoryV2ArchiveScalarV1::Null)) {
        references.push(reference(family, values, columns)?);
    }
    Ok(())
}

fn text_value<'a>(
    values: &'a BTreeMap<String, MemoryV2ArchiveScalarV1>,
    column: &str,
) -> Result<&'a str> {
    match values.get(column) {
        Some(MemoryV2ArchiveScalarV1::Text(value)) => Ok(value),
        _ => Err(db_message(
            OPERATION,
            format!("archive column `{column}` is not text"),
        )),
    }
}

fn table_specs() -> &'static [TableSpec] {
    use MemoryV2ArchiveFamilyV1 as F;
    use OwnerFilter as O;

    static SPECS: &[TableSpec] = &[
        TableSpec {
            family: F::RetrievalAnchor,
            table: "retrieval_anchors",
            columns: &[
                "anchor_id",
                "anchor_json",
                "owner_json",
                "projection_generation",
            ],
            key_columns: &["anchor_id"],
            owner_filter: O::RetrievalAnchor,
        },
        TableSpec {
            family: F::RetrievalAnchorAlias,
            table: "retrieval_anchor_aliases",
            columns: &["owner_json", "alias_kind", "locator_digest", "anchor_id"],
            key_columns: &["owner_json", "alias_kind", "locator_digest"],
            owner_filter: O::RetrievalAnchorChild("anchor_id"),
        },
        TableSpec {
            family: F::RetrievalAnchorDisposition,
            table: "retrieval_anchor_dispositions",
            columns: &[
                "disposition_id",
                "anchor_id",
                "owner_json",
                "state",
                "superseded_by",
                "reason_class",
                "effective_at",
                "record_json",
            ],
            key_columns: &["owner_json", "disposition_id"],
            owner_filter: O::RetrievalAnchorChild("anchor_id"),
        },
        TableSpec {
            family: F::RetrievalAnchorReverseLineage,
            table: "retrieval_anchor_reverse_lineage",
            columns: &[
                "source_anchor_id",
                "owner_json",
                "derivative_kind",
                "derivative_id",
                "direct_evidence",
            ],
            key_columns: &[
                "source_anchor_id",
                "owner_json",
                "derivative_kind",
                "derivative_id",
            ],
            owner_filter: O::RetrievalAnchorChild("source_anchor_id"),
        },
        TableSpec {
            family: F::RetrievalAnchorDerivativeTombstone,
            table: "retrieval_anchor_derivative_tombstones",
            columns: &[
                "source_anchor_id",
                "owner_json",
                "derivative_kind",
                "derivative_id",
                "disposition_id",
                "effective_at",
            ],
            key_columns: &[
                "source_anchor_id",
                "owner_json",
                "derivative_kind",
                "derivative_id",
                "disposition_id",
            ],
            owner_filter: O::RetrievalAnchorChild("source_anchor_id"),
        },
        TableSpec {
            family: F::EvidenceSourceOccurrence,
            table: "evidence_source_occurrences",
            columns: &[
                "occurrence_id",
                "owner_digest",
                "timeline_digest",
                "source_anchor_id",
                "source_order",
                "record_digest",
                "record_json",
            ],
            key_columns: &["occurrence_id"],
            owner_filter: O::EvidenceOwnerDigest,
        },
        TableSpec {
            family: F::EvidenceOccurrenceSet,
            table: "evidence_occurrence_sets",
            columns: &[
                "occurrence_set_id",
                "owner_digest",
                "record_digest",
                "record_json",
            ],
            key_columns: &["occurrence_set_id"],
            owner_filter: O::EvidenceOwnerDigest,
        },
        TableSpec {
            family: F::EvidenceOccurrenceSetMember,
            table: "evidence_occurrence_set_members",
            columns: &["occurrence_set_id", "canonical_ordinal", "occurrence_id"],
            key_columns: &["occurrence_set_id", "canonical_ordinal"],
            owner_filter: O::EvidenceParent("occurrence_set_id", "evidence_occurrence_sets"),
        },
        TableSpec {
            family: F::EvidenceSpan,
            table: "evidence_spans",
            columns: &[
                "span_id",
                "owner_digest",
                "occurrence_set_id",
                "anchor_id",
                "producer_kind",
                "record_digest",
                "record_json",
            ],
            key_columns: &["span_id"],
            owner_filter: O::EvidenceOwnerDigest,
        },
        TableSpec {
            family: F::EvidenceSpanMember,
            table: "evidence_span_members",
            columns: &[
                "span_id",
                "assembly_ordinal",
                "run_ordinal",
                "run_member_ordinal",
                "occurrence_id",
            ],
            key_columns: &["span_id", "assembly_ordinal"],
            owner_filter: O::EvidenceParent("span_id", "evidence_spans"),
        },
        TableSpec {
            family: F::EvidenceSpanProjectionReceipt,
            table: "evidence_span_projection_receipts",
            columns: &[
                "projection_receipt_id",
                "span_id",
                "record_digest",
                "record_json",
            ],
            key_columns: &["projection_receipt_id"],
            owner_filter: O::EvidenceParent("span_id", "evidence_spans"),
        },
        TableSpec {
            family: F::EvidenceRetrieverContribution,
            table: "evidence_retriever_contributions",
            columns: &[
                "contribution_id",
                "owner_digest",
                "span_id",
                "anchor_id",
                "record_digest",
                "record_json",
            ],
            key_columns: &["contribution_id"],
            owner_filter: O::EvidenceOwnerDigest,
        },
        TableSpec {
            family: F::EvidenceDerivedAnchor,
            table: "evidence_derived_anchors",
            columns: &[
                "anchor_id",
                "owner_digest",
                "target_kind",
                "target_id",
                "anchor_json",
            ],
            key_columns: &["anchor_id"],
            owner_filter: O::EvidenceOwnerDigest,
        },
        TableSpec {
            family: F::EvidenceAssemblyReceipt,
            table: "evidence_assembly_receipts",
            columns: &[
                "publication_receipt_id",
                "owner_digest",
                "privacy_domain_id",
                "key_epoch",
                "idempotency_key",
                "assembly_digest",
                "occurrence_set_id",
                "span_id",
                "contribution_id",
                "projection_receipt_id",
                "receipt_json",
            ],
            key_columns: &["publication_receipt_id"],
            owner_filter: O::EvidenceOwnerDigest,
        },
        TableSpec {
            family: F::Fact,
            table: "memory_v2_facts",
            columns: &[
                "fact_id",
                "owner_kind",
                "project_id",
                "owner_json",
                "identity_json",
                "created_at",
            ],
            key_columns: &["fact_id", "owner_kind", "project_id"],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::Assertion,
            table: "memory_v2_assertions",
            columns: &[
                "assertion_id",
                "fact_id",
                "owner_kind",
                "project_id",
                "owner_json",
                "assertion_header_json",
                "kind_json",
                "payload_reference_json",
                "receipt_json",
                "asserted_at",
                "actor_id",
            ],
            key_columns: &["assertion_id", "fact_id", "owner_kind", "project_id"],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::AssertionSupersession,
            table: "memory_v2_assertion_supersession",
            columns: &[
                "assertion_id",
                "fact_id",
                "owner_kind",
                "project_id",
                "superseded_assertion_id",
                "ordinal",
            ],
            key_columns: &[
                "assertion_id",
                "fact_id",
                "owner_kind",
                "project_id",
                "ordinal",
            ],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::AssertionPayload,
            table: "memory_v2_assertion_payloads",
            columns: &[
                "assertion_id",
                "fact_id",
                "owner_kind",
                "project_id",
                "payload_json",
                "content",
            ],
            key_columns: &["assertion_id", "fact_id", "owner_kind", "project_id"],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::AssertionVector,
            table: "memory_v2_assertion_vectors",
            columns: &[
                "assertion_id",
                "fact_id",
                "owner_kind",
                "project_id",
                "vector",
                "algebra",
                "dimensions",
                "precision",
            ],
            key_columns: &["assertion_id", "fact_id", "owner_kind", "project_id"],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::FactEvidence,
            table: "memory_v2_evidence",
            columns: &[
                "evidence_id",
                "fact_id",
                "owner_kind",
                "project_id",
                "owner_json",
                "anchor_id",
                "evidence_json",
            ],
            key_columns: &["evidence_id", "fact_id", "owner_kind", "project_id"],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::AssertionEvidence,
            table: "memory_v2_assertion_evidence",
            columns: &[
                "assertion_id",
                "evidence_id",
                "fact_id",
                "owner_kind",
                "project_id",
                "ordinal",
            ],
            key_columns: &[
                "assertion_id",
                "fact_id",
                "owner_kind",
                "project_id",
                "ordinal",
            ],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::LineageEvent,
            table: "memory_v2_lineage_events",
            columns: &[
                "event_id",
                "fact_id",
                "owner_kind",
                "project_id",
                "event_json",
                "occurred_at",
                "recorded_at",
            ],
            key_columns: &["event_id", "fact_id", "owner_kind", "project_id"],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::CurrentFact,
            table: "memory_v2_current_facts",
            columns: &[
                "fact_id",
                "owner_kind",
                "project_id",
                "payload_access",
                "trust_score",
                "active_assertion_id",
                "last_event_id",
                "updated_at",
                "retrieval_count",
                "access_count",
                "helpful_count",
                "unhelpful_count",
                "last_retrieved_at",
                "last_recalled_at",
                "last_feedback_at",
                "projection_state",
                "vector_watermark_json",
            ],
            key_columns: &["fact_id", "owner_kind", "project_id"],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::LegacyFactMap,
            table: "memory_v2_legacy_map",
            columns: &[
                "owner_kind",
                "project_id",
                "owner_json",
                "source_store_id",
                "legacy_fact_id",
                "fact_id",
                "mapping_json",
            ],
            key_columns: &[
                "owner_kind",
                "project_id",
                "source_store_id",
                "legacy_fact_id",
            ],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::LegacyQuarantine,
            table: "memory_v2_legacy_quarantine",
            columns: &[
                "owner_kind",
                "project_id",
                "source_store_id",
                "source_table",
                "source_row_id",
                "reason_code",
                "recorded_at",
            ],
            key_columns: &[
                "owner_kind",
                "project_id",
                "source_store_id",
                "source_table",
                "source_row_id",
            ],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::CompatibilityOperationReceipt,
            table: "memory_v2_compatibility_operation_receipts",
            columns: &[
                "owner_kind",
                "project_id",
                "operation_id",
                "operation_kind",
                "request_digest",
                "fact_id",
                "event_id",
                "receipt_json",
                "recorded_at",
            ],
            key_columns: &["owner_kind", "project_id", "operation_id"],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::LegacyFeedbackEventMap,
            table: "memory_v2_legacy_feedback_event_map",
            columns: &[
                "owner_kind",
                "project_id",
                "source_store_id",
                "legacy_feedback_event_id",
                "fact_id",
                "event_id",
            ],
            key_columns: &[
                "owner_kind",
                "project_id",
                "source_store_id",
                "legacy_feedback_event_id",
            ],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::FeedbackHistory,
            table: "memory_v2_feedback_history",
            columns: &[
                "owner_kind",
                "project_id",
                "fact_id",
                "event_id",
                "action",
                "old_trust",
                "new_trust",
                "occurred_at",
                "source",
                "note",
                "details_availability",
            ],
            key_columns: &["owner_kind", "project_id", "fact_id", "event_id"],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::FactRelation,
            table: "memory_v2_fact_relations",
            columns: &[
                "owner_kind",
                "project_id",
                "source_fact_id",
                "target_fact_id",
                "relation",
                "confidence",
                "source_label",
                "provenance_json",
                "evidence_fact_ids_json",
                "occurred_at",
                "updated_at",
            ],
            key_columns: &[
                "owner_kind",
                "project_id",
                "source_fact_id",
                "target_fact_id",
                "relation",
            ],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::Proposal,
            table: "memory_v2_proposals",
            columns: &[
                "proposal_id",
                "owner_kind",
                "project_id",
                "owner_json",
                "idempotency_key",
                "request_digest",
                "request_json",
                "evidence_json",
                "submitted_at",
            ],
            key_columns: &["proposal_id", "owner_kind", "project_id"],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::ProposalTransition,
            table: "memory_v2_proposal_transitions",
            columns: &[
                "transition_id",
                "proposal_id",
                "owner_kind",
                "project_id",
                "previous_state",
                "current_state",
                "reviewer_json",
                "validation_json",
                "origin",
                "promoted_fact_id",
                "promoted_assertion_id",
                "promoted_event_id",
                "transition_json",
                "occurred_at",
            ],
            key_columns: &["transition_id", "proposal_id", "owner_kind", "project_id"],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::ProposalCurrent,
            table: "memory_v2_proposal_current",
            columns: &[
                "proposal_id",
                "owner_kind",
                "project_id",
                "state",
                "revision",
                "last_transition_id",
                "updated_at",
            ],
            key_columns: &["proposal_id", "owner_kind", "project_id"],
            owner_filter: O::Scope,
        },
        TableSpec {
            family: F::LegacyProposalMap,
            table: "memory_v2_legacy_proposal_map",
            columns: &[
                "owner_kind",
                "project_id",
                "source_store_id",
                "legacy_proposal_id",
                "proposal_id",
                "history_coverage",
                "import_receipt_json",
                "imported_at",
            ],
            key_columns: &[
                "owner_kind",
                "project_id",
                "source_store_id",
                "legacy_proposal_id",
            ],
            owner_filter: O::Scope,
        },
    ];
    SPECS
}

fn validate_table_specs() -> Result<()> {
    let expected = authoritative_memory_v2_archive_families();
    let actual: BTreeSet<_> = table_specs().iter().map(|spec| spec.family).collect();
    if actual != expected || actual.len() != table_specs().len() {
        return Err(db_message(
            OPERATION,
            "authoritative archive families and physical table adapters are not one-to-one",
        ));
    }
    let tables: BTreeSet<_> = table_specs().iter().map(|spec| spec.table).collect();
    if tables.len() != table_specs().len() {
        return Err(db_message(
            OPERATION,
            "authoritative archive table adapters contain duplicate physical tables",
        ));
    }
    for spec in table_specs() {
        let columns: BTreeSet<_> = spec.columns.iter().copied().collect();
        let keys: BTreeSet<_> = spec.key_columns.iter().copied().collect();
        if columns.len() != spec.columns.len()
            || keys.len() != spec.key_columns.len()
            || !keys.is_subset(&columns)
        {
            return Err(db_message(
                OPERATION,
                format!(
                    "archive {:?} has an invalid physical schema contract",
                    spec.family
                ),
            ));
        }
    }
    Ok(())
}

async fn validate_physical_schema(
    conn: &(impl Executor + Sync),
    database: MemoryV2ArchiveDatabase,
) -> Result<()> {
    for spec in table_specs() {
        let sql = format!(
            "PRAGMA {}.table_info('{}')",
            database.schema_name(),
            spec.table
        );
        let mut rows = conn
            .query(&sql, ())
            .await
            .map_err(|error| db_error(OPERATION, error))?;
        let mut physical_columns = Vec::new();
        let mut physical_primary_key = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| db_error(OPERATION, error))?
        {
            let name: String = row.get(1).map_err(|error| db_error(OPERATION, error))?;
            let primary_key_ordinal: i64 =
                row.get(5).map_err(|error| db_error(OPERATION, error))?;
            physical_columns.push(name.clone());
            if primary_key_ordinal > 0 {
                physical_primary_key.push((primary_key_ordinal, name));
            }
        }
        let local_columns = target_local_physical_columns(spec.family);
        let expected_columns = local_columns
            .iter()
            .copied()
            .chain(spec.columns.iter().copied())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if physical_columns != expected_columns {
            return Err(db_message(
                OPERATION,
                format!(
                    "archive {:?} physical columns drifted: expected {expected_columns:?}, got {physical_columns:?}",
                    spec.family
                ),
            ));
        }
        physical_primary_key.sort_by_key(|(ordinal, _)| *ordinal);
        let physical_primary_key = physical_primary_key
            .into_iter()
            .map(|(_, column)| column)
            .collect::<Vec<_>>();
        let expected_key = if local_columns.is_empty() {
            spec.key_columns
        } else {
            local_columns
        }
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        if !physical_primary_key.is_empty() && physical_primary_key != expected_key {
            return Err(db_message(
                OPERATION,
                format!(
                    "archive {:?} physical primary key drifted: expected {expected_key:?}, got {physical_primary_key:?}",
                    spec.family
                ),
            ));
        }
        if !local_columns.is_empty() && !has_unique_logical_key(conn, database, spec).await? {
            return Err(db_message(
                OPERATION,
                format!(
                    "archive {:?} logical identity is not backed by an exact unique index",
                    spec.family
                ),
            ));
        }
    }
    Ok(())
}

async fn has_unique_logical_key(
    conn: &(impl Executor + Sync),
    database: MemoryV2ArchiveDatabase,
    spec: &TableSpec,
) -> Result<bool> {
    let sql = format!(
        "PRAGMA {}.index_list('{}')",
        database.schema_name(),
        spec.table
    );
    let mut rows = conn
        .query(&sql, ())
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut unique_indexes = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let name: String = row.get(1).map_err(|error| db_error(OPERATION, error))?;
        let unique: i64 = row.get(2).map_err(|error| db_error(OPERATION, error))?;
        if unique != 0 {
            unique_indexes.push(name);
        }
    }
    drop(rows);
    for index in unique_indexes {
        let sql = format!(
            "PRAGMA {}.index_info('{}')",
            database.schema_name(),
            index.replace('\'', "''")
        );
        let mut rows = conn
            .query(&sql, ())
            .await
            .map_err(|error| db_error(OPERATION, error))?;
        let mut columns = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| db_error(OPERATION, error))?
        {
            columns.push(
                row.get::<String>(2)
                    .map_err(|error| db_error(OPERATION, error))?,
            );
        }
        if columns
            == spec
                .key_columns
                .iter()
                .map(|column| (*column).to_owned())
                .collect::<Vec<_>>()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn target_local_physical_columns(family: MemoryV2ArchiveFamilyV1) -> &'static [&'static str] {
    match family {
        MemoryV2ArchiveFamilyV1::RetrievalAnchorDisposition => &["sequence"],
        MemoryV2ArchiveFamilyV1::AssertionPayload => &["rowid"],
        MemoryV2ArchiveFamilyV1::LineageEvent => &["event_sequence"],
        MemoryV2ArchiveFamilyV1::ProposalTransition => &["transition_sequence"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_adapters_cover_each_authoritative_family_once() {
        validate_table_specs().unwrap();
        assert_eq!(
            table_specs().len(),
            authoritative_memory_v2_archive_families().len()
        );
    }

    #[test]
    fn every_authoritative_family_has_an_owner_filter_contract() {
        for spec in table_specs() {
            let main = owner_predicate(MemoryV2ArchiveDatabase::Main, spec.owner_filter);
            let source = owner_predicate(MemoryV2ArchiveDatabase::Source, spec.owner_filter);
            assert!(!main.trim().is_empty(), "{:?}", spec.family);
            assert!(!source.trim().is_empty(), "{:?}", spec.family);
            assert!(
                main.contains("?1") || main.contains("?3"),
                "{:?} owner filter is not parameter-bound",
                spec.family
            );
        }
    }

    #[test]
    fn physical_adapter_rejects_unknown_or_missing_fields_before_sql() {
        let spec = table_specs()
            .iter()
            .find(|spec| spec.family == MemoryV2ArchiveFamilyV1::Fact)
            .unwrap();
        let record = MemoryV2ArchiveRecordV1::new(
            MemoryV2ArchiveFamilyV1::Fact,
            BTreeMap::from([
                (
                    "fact_id".to_owned(),
                    MemoryV2ArchiveScalarV1::Text("fact.adapter".to_owned()),
                ),
                (
                    "owner_kind".to_owned(),
                    MemoryV2ArchiveScalarV1::Text("profile".to_owned()),
                ),
                (
                    "project_id".to_owned(),
                    MemoryV2ArchiveScalarV1::Text(String::new()),
                ),
            ]),
            BTreeMap::from([(
                "unexpected".to_owned(),
                MemoryV2ArchiveScalarV1::Text("value".to_owned()),
            )]),
            Vec::new(),
        )
        .unwrap();
        assert!(validate_record_fields(spec, &record).is_err());
    }
}
