use std::collections::BTreeMap;

use super::*;
use tracedecay_store::{StoreAuthorityEpochV1, StoreRuntimeBindingV1};

pub(super) fn all_families() -> [StoreFamily; 4] {
    [
        StoreFamily::Profile,
        StoreFamily::Project,
        StoreFamily::ProjectSessions,
        StoreFamily::Code,
    ]
}

pub(super) fn family_digest(family: StoreFamily, final_schema: bool) -> Digest {
    let base = if final_schema { 20 } else { 10 };
    Digest([base + family as u8; 32])
}

fn manifest(final_schema: bool) -> SchemaManifest {
    let families = all_families()
        .into_iter()
        .map(|family| {
            (
                family,
                FamilySchemaManifest {
                    family,
                    schema_digest: family_digest(family, final_schema),
                    transform_revision: if final_schema { 2 } else { 1 },
                },
            )
        })
        .collect();
    SchemaManifest {
        schema_id: SchemaId::new(if final_schema {
            "schema.final"
        } else {
            "schema.last"
        })
        .unwrap(),
        revision: if final_schema { 2 } else { 1 },
        canonical_digest: if final_schema { FINAL } else { LAST },
        families,
    }
}

pub(super) fn request() -> MigrationRequest {
    let source_bindings = all_families()
        .into_iter()
        .map(|family| {
            let scope = match family {
                StoreFamily::Profile => serde_json::json!({ "kind": "profile" }),
                StoreFamily::Project => serde_json::json!({
                    "kind": "project", "project_id": "project.migration"
                }),
                StoreFamily::ProjectSessions => serde_json::json!({
                    "kind": "project_sessions", "project_id": "project.migration"
                }),
                StoreFamily::Code => serde_json::json!({
                    "kind": "code",
                    "project_id": "project.migration",
                    "repository_id": "repository.migration",
                    "scope": { "kind": "worktree", "worktree_id": "worktree.migration" }
                }),
            };
            let binding: StoreRuntimeBindingV1 = serde_json::from_value(serde_json::json!({
                "shard_id": {
                    "brain_id": "brain.migration",
                    "profile_id": "profile.migration",
                    "scope": scope
                },
                "incarnation": 1,
                "authority_epoch": 7
            }))
            .unwrap();
            (family, vec![binding])
        })
        .collect();
    MigrationRequest {
        migration_id: MigrationId::new("migration.release-v2").unwrap(),
        source_bindings,
        destination_epoch: StoreAuthorityEpochV1::new(8).unwrap(),
        freeze_proof: ReleaseFreezeProof {
            acceptance_id: FreezeAcceptanceId::new("release.accepted").unwrap(),
            last_released_digest: LAST,
            final_digest: FINAL,
            proof_digest: EVIDENCE,
        },
    }
}

pub(super) fn engine(port: FakePort) -> ConsolidatedMigrationEngine<FakePort> {
    ConsolidatedMigrationEngine::new(
        port,
        LastReleasedSchemaManifest::new(manifest(false)).unwrap(),
        FinalSchemaManifest::new(manifest(true)).unwrap(),
    )
    .unwrap()
}

pub(super) fn family_transforms() -> BTreeMap<StoreFamily, FakeTransform> {
    all_families()
        .into_iter()
        .map(|family| {
            (
                family,
                FakeTransform {
                    family,
                    applications: 0,
                },
            )
        })
        .collect()
}
