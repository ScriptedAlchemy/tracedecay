use std::fmt;

use serde::Deserialize;
use serde::de::{MapAccess, SeqAccess, Visitor};

pub(super) struct ClosedJsonValue(pub(super) serde_json::Value);

impl<'de> Deserialize<'de> for ClosedJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ClosedJsonValueVisitor;

        impl<'de> Visitor<'de> for ClosedJsonValueVisitor {
            type Value = ClosedJsonValue;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value without duplicate object keys")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(ClosedJsonValue(serde_json::Value::Bool(value)))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(ClosedJsonValue(serde_json::Value::Number(value.into())))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(ClosedJsonValue(serde_json::Value::Number(value.into())))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                serde_json::Number::from_f64(value)
                    .map(serde_json::Value::Number)
                    .map(ClosedJsonValue)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_string(value.to_owned())
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(ClosedJsonValue(serde_json::Value::String(value)))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(ClosedJsonValue(serde_json::Value::Null))
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                ClosedJsonValue::deserialize(deserializer)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(ClosedJsonValue(serde_json::Value::Null))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<ClosedJsonValue>()? {
                    values.push(value.0);
                }
                Ok(ClosedJsonValue(serde_json::Value::Array(values)))
            }

            fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = serde_json::Map::new();
                while let Some(key) = entries.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!("duplicate field `{key}`")));
                    }
                    let value = entries.next_value::<ClosedJsonValue>()?;
                    values.insert(key, value.0);
                }
                Ok(ClosedJsonValue(serde_json::Value::Object(values)))
            }
        }

        deserializer.deserialize_any(ClosedJsonValueVisitor)
    }
}

type StrictWireResult<T = ()> = Result<T, String>;

fn strict_object<'a>(
    value: &'a serde_json::Value,
    allowed: &[&str],
    path: &str,
) -> StrictWireResult<Option<&'a serde_json::Map<String, serde_json::Value>>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(format!("unknown field `{field}` at {path}"));
    }
    Ok(Some(object))
}

fn strict_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    path: &str,
    check: fn(&serde_json::Value, &str) -> StrictWireResult,
) -> StrictWireResult {
    if let Some(value) = object.get(field) {
        check(value, &format!("{path}.{field}"))?;
    }
    Ok(())
}

fn strict_array(
    value: &serde_json::Value,
    path: &str,
    check: fn(&serde_json::Value, &str) -> StrictWireResult,
) -> StrictWireResult {
    if let Some(values) = value.as_array() {
        for (index, value) in values.iter().enumerate() {
            check(value, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

fn strict_map_values(
    value: &serde_json::Value,
    path: &str,
    check: fn(&serde_json::Value, &str) -> StrictWireResult,
) -> StrictWireResult {
    if let Some(values) = value.as_object() {
        for (key, value) in values {
            check(value, &format!("{path}.{key}"))?;
        }
    }
    Ok(())
}

fn strict_sanitization_receipt(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["receipt_id", "sanitizer_version"], path)?;
    Ok(())
}

fn strict_audit_receipt(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["receipt_id"], path)?;
    Ok(())
}

fn strict_log_safe_text(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["receipt", "value"], path)? else {
        return Ok(());
    };
    strict_field(object, "receipt", path, strict_sanitization_receipt)
}

fn strict_vector_watermark(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["components"], path)?;
    Ok(())
}

fn strict_shard_watermark(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["outbox_sequence", "shard_id"], path)?;
    Ok(())
}

fn strict_catalog_snapshot(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["digest", "generation"], path)?;
    Ok(())
}

fn strict_entity_kind(value: &serde_json::Value, path: &str) -> StrictWireResult {
    if !value.is_object() {
        return Ok(());
    }
    let Some(object) = strict_object(value, &["other"], path)? else {
        return Ok(());
    };
    strict_field(object, "other", path, strict_log_safe_text)
}

fn strict_entity_ref(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["id", "kind"], path)? else {
        return Ok(());
    };
    strict_field(object, "kind", path, strict_entity_kind)
}

fn strict_entity_version_ref(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["entity", "version"], path)? else {
        return Ok(());
    };
    strict_field(object, "entity", path, strict_entity_ref)
}

fn strict_actor_ref(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["actor_id", "version"], path)?;
    Ok(())
}

fn strict_time_interval(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["end", "start"], path)?;
    Ok(())
}

fn strict_source_position(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let allowed = match object.get("kind").and_then(serde_json::Value::as_str) {
        Some("byte_offset") => &["end", "kind", "start"][..],
        Some("row_id") => &["kind", "row_id"][..],
        Some("sequence") => &["kind", "sequence"][..],
        Some("object_key") => &["digest", "kind"][..],
        _ => return Ok(()),
    };
    strict_object(value, allowed, path)?;
    Ok(())
}

fn strict_activity_facet(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(
        value,
        &[
            "agent_instance_id",
            "goal_id",
            "host",
            "message_id",
            "orchestration_agent_label",
            "orchestration_observation_id",
            "parent_session_id",
            "parent_tool_use_id",
            "provider",
            "session_id",
            "source_store_id",
            "thread_id",
            "turn_id",
        ],
        path,
    )?;
    Ok(())
}

fn strict_git_subject(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(
        value,
        &[
            "commit_id",
            "project_id",
            "ref_id",
            "repository_id",
            "worktree_id",
        ],
        path,
    )?;
    Ok(())
}

fn strict_delivery_subject(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["delivery_entity", "repository_id"], path)? else {
        return Ok(());
    };
    strict_field(object, "delivery_entity", path, strict_entity_ref)
}

fn strict_source_subject(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &["source_entity", "source_position", "source_store_id"],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "source_entity", path, strict_entity_ref)?;
    strict_field(object, "source_position", path, strict_source_position)
}

fn strict_web_subject(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["captured_document", "source_manifest"], path)?
    else {
        return Ok(());
    };
    strict_field(object, "captured_document", path, strict_entity_ref)?;
    strict_field(object, "source_manifest", path, strict_entity_ref)
}

fn strict_document_subject(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["document", "version"], path)? else {
        return Ok(());
    };
    strict_field(object, "document", path, strict_entity_ref)?;
    strict_field(object, "version", path, strict_entity_version_ref)
}

fn strict_research_subject(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["kind", "subject"], path)? else {
        return Ok(());
    };
    let Some(subject) = object.get("subject") else {
        return Ok(());
    };
    let subject_path = format!("{path}.subject");
    match object.get("kind").and_then(serde_json::Value::as_str) {
        Some("activity") => strict_activity_facet(subject, &subject_path),
        Some("git") => strict_git_subject(subject, &subject_path),
        Some("delivery") => strict_delivery_subject(subject, &subject_path),
        Some("source") => strict_source_subject(subject, &subject_path),
        Some("web") => strict_web_subject(subject, &subject_path),
        Some("document") => strict_document_subject(subject, &subject_path),
        _ => Ok(()),
    }
}

fn strict_evidence_retention(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(value, &["cutoffs", "evaluated_at"], path)?;
    Ok(())
}

fn strict_read_consistency(value: &serde_json::Value, path: &str) -> StrictWireResult {
    if !value.is_object() {
        return Ok(());
    }
    let Some(object) = strict_object(value, &["bounded_stale"], path)? else {
        return Ok(());
    };
    if let Some(bounded) = object.get("bounded_stale") {
        strict_object(
            bounded,
            &["max_lag_micros"],
            &format!("{path}.bounded_stale"),
        )?;
    }
    Ok(())
}

fn strict_remote_shard_coverage(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "authority_epoch",
            "authority_id",
            "cache_age_micros",
            "cache_generation",
            "cache_not_after",
            "captured_watermark",
            "pending_local_observations",
            "pending_tombstone_acks",
            "served_by_node",
            "served_by_role",
            "shard_id",
            "sync_lag_micros",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "captured_watermark", path, strict_shard_watermark)
}

fn strict_remote_coverage(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "brain_id",
            "placement_version",
            "requested_consistency",
            "shards",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(
        object,
        "requested_consistency",
        path,
        strict_read_consistency,
    )?;
    if let Some(shards) = object.get("shards") {
        strict_array(
            shards,
            &format!("{path}.shards"),
            strict_remote_shard_coverage,
        )?;
    }
    Ok(())
}

fn strict_coverage(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "freshness",
            "incompatible",
            "locked",
            "redacted",
            "remote",
            "retention_watermark",
            "searched",
            "skipped",
            "stale",
            "truncated",
            "unavailable",
            "unknown_coverage",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    if let Some(freshness) = object.get("freshness") {
        strict_map_values(
            freshness,
            &format!("{path}.freshness"),
            strict_shard_watermark,
        )?;
    }
    strict_field(
        object,
        "retention_watermark",
        path,
        strict_evidence_retention,
    )?;
    strict_field(object, "remote", path, strict_remote_coverage)
}

fn strict_private_corpus(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "manifest_digest",
            "manifest_id",
            "privacy_domain",
            "source_watermark",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "source_watermark", path, strict_vector_watermark)
}

fn strict_git_truth(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(
        value,
        &[
            "captured_at",
            "dirty",
            "head_commit",
            "merge_base",
            "refs",
            "repository",
        ],
        path,
    )?;
    Ok(())
}

fn strict_contribution(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "confidence",
            "contributor",
            "evidence_class",
            "manifest_entries",
            "outputs",
            "role",
            "session_id",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "contributor", path, strict_actor_ref)?;
    if let Some(outputs) = object.get("outputs") {
        strict_array(outputs, &format!("{path}.outputs"), strict_entity_ref)?;
    }
    Ok(())
}

fn strict_attribution_gap(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &["candidate_sessions", "reason", "repair_recipe", "subject"],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "subject", path, strict_log_safe_text)
}

fn strict_redaction_report(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(
        value,
        &[
            "receipts",
            "redacted",
            "rejected",
            "sanitizer_version",
            "scanned",
        ],
        path,
    )?;
    Ok(())
}

fn strict_retrieval_target(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["kind", "target"], path)? else {
        return Ok(());
    };
    let Some(target) = object.get("target") else {
        return Ok(());
    };
    let target_path = format!("{path}.target");
    match object.get("kind").and_then(serde_json::Value::as_str) {
        Some("entity") => strict_entity_ref(target, &target_path),
        Some("source_position") => {
            let Some(target) = strict_object(target, &["position_digest", "source"], &target_path)?
            else {
                return Ok(());
            };
            strict_field(target, "source", &target_path, strict_entity_ref)
        }
        Some("artifact") => {
            let Some(target) = strict_object(
                target,
                &["artifact", "sanitized_output_digest"],
                &target_path,
            )?
            else {
                return Ok(());
            };
            strict_field(target, "artifact", &target_path, strict_entity_ref)
        }
        _ => Ok(()),
    }
}

fn strict_expansion_recipe(value: &serde_json::Value, path: &str) -> StrictWireResult {
    strict_object(
        value,
        &["bounded_arguments_digest", "capability_id", "expansion"],
        path,
    )?;
    Ok(())
}

fn strict_anchor_durability(value: &serde_json::Value, path: &str) -> StrictWireResult {
    if !value.is_object() {
        return Ok(());
    }
    let Some(object) = strict_object(value, &["retention_bound"], path)? else {
        return Ok(());
    };
    if let Some(retention_bound) = object.get("retention_bound") {
        strict_object(
            retention_bound,
            &["expires_at"],
            &format!("{path}.retention_bound"),
        )?;
    }
    Ok(())
}

fn strict_retrieval_record(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "access_policy_digest",
            "anchor_id",
            "canonical_request_digest",
            "capability_catalog",
            "created_at",
            "data_version_digest",
            "durability",
            "expansion_recipe",
            "immutable_source_refs",
            "payload_access",
            "privacy_domain_id",
            "projection_version",
            "provenance",
            "resolved_scope_id",
            "retention_class",
            "schema_registry_digest",
            "snapshot",
            "source_identity_class",
            "source_observations",
            "target",
            "target_kind",
            "view",
            "view_algorithm_version",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "capability_catalog", path, strict_catalog_snapshot)?;
    strict_field(object, "durability", path, strict_anchor_durability)?;
    strict_field(object, "expansion_recipe", path, strict_expansion_recipe)?;
    if let Some(sources) = object.get("immutable_source_refs") {
        strict_array(
            sources,
            &format!("{path}.immutable_source_refs"),
            strict_entity_ref,
        )?;
    }
    strict_field(object, "snapshot", path, strict_vector_watermark)?;
    strict_field(object, "target", path, strict_retrieval_target)?;
    strict_field(object, "target_kind", path, strict_entity_kind)
}

fn strict_retrieval_catalog(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(value, &["records", "snapshot"], path)? else {
        return Ok(());
    };
    if let Some(records) = object.get("records") {
        strict_map_values(records, &format!("{path}.records"), strict_retrieval_record)?;
    }
    strict_field(object, "snapshot", path, strict_catalog_snapshot)
}

fn strict_retrieval_recipe(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &["anchors", "purpose", "recipe_id", "snapshot", "use_case"],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "purpose", path, strict_log_safe_text)?;
    strict_field(object, "snapshot", path, strict_vector_watermark)
}

fn strict_research_anchor(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "confidence",
            "coverage",
            "entry_id",
            "evidence_class",
            "expected_subject",
            "occurred_window",
            "purpose",
            "related_activity",
            "retrieval_anchors",
            "retrieval_recipe_id",
            "snapshot",
            "source_observation_ids",
            "subject",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "coverage", path, strict_coverage)?;
    strict_field(object, "expected_subject", path, strict_log_safe_text)?;
    strict_field(object, "occurred_window", path, strict_time_interval)?;
    strict_field(object, "purpose", path, strict_log_safe_text)?;
    strict_field(object, "related_activity", path, strict_activity_facet)?;
    strict_field(object, "snapshot", path, strict_vector_watermark)?;
    strict_field(object, "subject", path, strict_research_subject)
}

fn strict_manifest(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "agent_contributions",
            "anchors",
            "base_commit",
            "catalog_snapshot",
            "created_at",
            "created_by",
            "digest",
            "git_snapshot",
            "manifest_id",
            "parent_plan",
            "plan_commit",
            "private_corpus",
            "redaction_report",
            "repository",
            "retrieval_recipes",
            "schema_version",
            "store_watermarks",
            "supersedes",
            "unresolved_attribution",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    if let Some(contributions) = object.get("agent_contributions") {
        strict_array(
            contributions,
            &format!("{path}.agent_contributions"),
            strict_contribution,
        )?;
    }
    if let Some(anchors) = object.get("anchors") {
        strict_array(anchors, &format!("{path}.anchors"), strict_research_anchor)?;
    }
    strict_field(object, "catalog_snapshot", path, strict_catalog_snapshot)?;
    strict_field(object, "created_by", path, strict_actor_ref)?;
    strict_field(object, "git_snapshot", path, strict_git_truth)?;
    strict_field(object, "parent_plan", path, strict_entity_ref)?;
    strict_field(object, "private_corpus", path, strict_private_corpus)?;
    strict_field(object, "redaction_report", path, strict_redaction_report)?;
    if let Some(recipes) = object.get("retrieval_recipes") {
        strict_array(
            recipes,
            &format!("{path}.retrieval_recipes"),
            strict_retrieval_recipe,
        )?;
    }
    strict_field(object, "store_watermarks", path, strict_vector_watermark)?;
    if let Some(gaps) = object.get("unresolved_attribution") {
        strict_array(
            gaps,
            &format!("{path}.unresolved_attribution"),
            strict_attribution_gap,
        )?;
    }
    Ok(())
}

pub(super) fn strict_tombstone(value: &serde_json::Value, path: &str) -> StrictWireResult {
    let Some(object) = strict_object(
        value,
        &[
            "coverage",
            "entry_id",
            "evidence_class",
            "occurred_at",
            "reason",
            "receipt",
            "retrieval_anchors",
            "snapshot",
            "subject",
        ],
        path,
    )?
    else {
        return Ok(());
    };
    strict_field(object, "coverage", path, strict_coverage)?;
    strict_field(object, "receipt", path, strict_audit_receipt)?;
    strict_field(object, "snapshot", path, strict_vector_watermark)?;
    strict_field(object, "subject", path, strict_research_subject)
}

pub(super) fn strict_envelope(value: &serde_json::Value) -> StrictWireResult {
    let Some(object) = strict_object(value, &["manifest", "retrieval_catalog"], "envelope")? else {
        return Ok(());
    };
    strict_field(object, "manifest", "envelope", strict_manifest)?;
    strict_field(
        object,
        "retrieval_catalog",
        "envelope",
        strict_retrieval_catalog,
    )
}
