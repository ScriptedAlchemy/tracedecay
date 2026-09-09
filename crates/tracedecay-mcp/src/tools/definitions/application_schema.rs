//! Closed JSON-object schema construction shared by application tools, and the
//! projection every canonical executable request schema goes through before it
//! is advertised as an MCP `inputSchema`.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use tracedecay_tool_catalog::{ExecutableBindingRegistryV1, OperationId};

use super::required_object_schema;

type DiscoveryResult<T> = Result<T, crate::McpCatalogError>;

pub(super) fn canonical_application_request_schema(
    registry: &ExecutableBindingRegistryV1,
    operation: &'static str,
) -> DiscoveryResult<Value> {
    let operation_id = OperationId::new(format!("operation.application.{operation}"))
        .map_err(|_| invalid_terminal_application_discovery())?;
    registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
        .map(|binding| binding.request_schema().body().clone())
        .ok_or_else(invalid_terminal_application_discovery)
}

fn invalid_terminal_application_discovery() -> crate::McpCatalogError {
    tracedecay_tool_catalog::CatalogValidationError::InvalidValue {
        field: "terminal application MCP executable binding",
        reason: "must expose the canonical executable request schema",
    }
    .into()
}

pub(super) fn closed_object_schema(
    properties: serde_json::Value,
    required: &[&str],
) -> serde_json::Value {
    let mut schema = required_object_schema(properties, required);
    schema["additionalProperties"] = json!(false);
    schema
}

/// Project a canonical executable request schema into the MCP `inputSchema`
/// advertised for a tool.
///
/// Validation semantics are unchanged. `tools/list` repeats every schema body
/// once per tool, so the projection drops only what an embedded tool schema
/// never uses: the `$schema` dialect marker, the root `title` (the generating
/// Rust type name, which SDK generation reads from the executable binding's
/// Rust type path instead), and the paragraphs after each description's first
/// one. Those trailing paragraphs are the rustdoc rationale of the wire type;
/// the summary paragraph is the model-facing description.
#[must_use]
pub fn mcp_input_schema(canonical: &Value) -> Value {
    let mut schema = canonical.clone();
    project_input_schema(&mut schema);
    schema
}

pub(super) fn project_input_schema(schema: &mut Value) {
    if let Some(root) = schema.as_object_mut() {
        root.remove("$schema");
        root.remove("title");
    }
    summarize_descriptions(schema);
}

fn summarize_descriptions(value: &mut Value) {
    match value {
        Value::Object(members) => {
            for (key, member) in members.iter_mut() {
                if key == "description"
                    && let Value::String(text) = member
                {
                    if let Some(summary) = first_paragraph(text) {
                        *text = summary;
                    }
                } else {
                    summarize_descriptions(member);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(summarize_descriptions),
        _ => {}
    }
}

/// The text before the first blank line, or `None` when the description is a
/// single paragraph and stays byte-identical.
fn first_paragraph(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let mut lines = trimmed.lines();
    let first_blank = lines.position(|line| line.trim().is_empty())?;
    Some(
        trimmed
            .lines()
            .take(first_blank)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned(),
    )
}

/// Replace one adjacently tagged union in `$defs` with its discriminator and an
/// open payload, then drop the definitions only that union referenced.
///
/// The discriminator values are read from the canonical branches, so the
/// bounded schema still names every admissible tag; the payload shape is
/// validated by the daemon on admission exactly as before. Callers use this
/// for CAS-gated writes whose typed value the agent has just read back, where
/// re-advertising the complete value union in every `tools/list` would cost
/// tens of kilobytes per tool without telling the model anything the read did
/// not.
pub(super) fn bound_tagged_union(
    schema: &mut Value,
    definition: &str,
    payload_guidance: &str,
) -> DiscoveryResult<()> {
    let union = schema
        .get("$defs")
        .and_then(|definitions| definitions.get(definition))
        .ok_or_else(|| bounded_union_error("bounded union must be a `$defs` entry"))?;
    let branches = union
        .get("oneOf")
        .and_then(Value::as_array)
        .filter(|branches| !branches.is_empty())
        .ok_or_else(|| bounded_union_error("bounded union must be a non-empty `oneOf`"))?;

    let mut tag_property: Option<&str> = None;
    let mut payload_property: Option<&str> = None;
    let mut tags = Vec::with_capacity(branches.len());
    for branch in branches {
        let properties = branch
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| bounded_union_error("every union branch must be an object"))?;
        let mut branch_tag: Option<(&str, &str)> = None;
        for (name, property) in properties {
            match property.get("const") {
                Some(Value::String(tag)) => {
                    if branch_tag.replace((name.as_str(), tag.as_str())).is_some() {
                        return Err(bounded_union_error(
                            "every union branch must carry exactly one tag",
                        ));
                    }
                }
                Some(_) => {
                    return Err(bounded_union_error("union tags must be strings"));
                }
                None => {
                    if payload_property
                        .replace(name.as_str())
                        .is_some_and(|prior| prior != name.as_str())
                    {
                        return Err(bounded_union_error(
                            "every union branch must share one payload property",
                        ));
                    }
                }
            }
        }
        let (tag_name, tag) =
            branch_tag.ok_or_else(|| bounded_union_error("every union branch must be tagged"))?;
        if tag_property
            .replace(tag_name)
            .is_some_and(|prior| prior != tag_name)
        {
            return Err(bounded_union_error("every union branch must share one tag"));
        }
        tags.push(Value::String(tag.to_owned()));
    }
    let tag_property = tag_property
        .ok_or_else(|| bounded_union_error("every union branch must be tagged"))?
        .to_owned();
    let payload_property = payload_property
        .ok_or_else(|| bounded_union_error("bounded union must carry a payload"))?
        .to_owned();
    let mut description = union
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default();
    if !description.is_empty() {
        description.push(' ');
    }
    description.push_str(payload_guidance);

    let mut properties = Map::new();
    properties.insert(
        tag_property.clone(),
        json!({ "type": "string", "enum": tags }),
    );
    properties.insert(
        payload_property.clone(),
        json!({ "description": format!("Typed payload selected by `{tag_property}`.") }),
    );
    let bounded = json!({
        "type": "object",
        "description": description,
        "properties": properties,
        "required": [tag_property, payload_property],
        "additionalProperties": false
    });
    let definitions = schema
        .get_mut("$defs")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| bounded_union_error("bounded union must be a `$defs` entry"))?;
    definitions.insert(definition.to_owned(), bounded);
    retain_referenced_definitions(schema);
    Ok(())
}

fn bounded_union_error(reason: &'static str) -> crate::McpCatalogError {
    tracedecay_tool_catalog::CatalogValidationError::InvalidValue {
        field: "bounded application MCP request schema",
        reason,
    }
    .into()
}

/// Keep only the `$defs` entries reachable from the schema body.
fn retain_referenced_definitions(schema: &mut Value) {
    let Some(root) = schema.as_object_mut() else {
        return;
    };
    let Some(Value::Object(definitions)) = root.remove("$defs") else {
        return;
    };
    let mut reachable = BTreeSet::new();
    let mut pending = BTreeSet::new();
    for member in root.values() {
        collect_definition_references(member, &mut pending);
    }
    while let Some(name) = pending.pop_first() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        if let Some(definition) = definitions.get(&name) {
            let mut nested = BTreeSet::new();
            collect_definition_references(definition, &mut nested);
            pending.extend(nested.difference(&reachable).cloned());
        }
    }
    let retained = definitions
        .into_iter()
        .filter(|(name, _)| reachable.contains(name))
        .collect::<Map<_, _>>();
    if !retained.is_empty() {
        root.insert("$defs".to_owned(), Value::Object(retained));
    }
}

fn collect_definition_references(value: &Value, references: &mut BTreeSet<String>) {
    const DEFINITIONS_PREFIX: &str = "#/$defs/";
    match value {
        Value::Object(members) => {
            for (key, member) in members {
                if key == "$ref"
                    && let Value::String(target) = member
                    && let Some(name) = target.strip_prefix(DEFINITIONS_PREFIX)
                {
                    references.insert(name.to_owned());
                } else {
                    collect_definition_references(member, references);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_definition_references(item, references);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{bound_tagged_union, mcp_input_schema};

    #[test]
    fn projection_drops_document_metadata_and_rationale_paragraphs() {
        let canonical = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "ExampleRequestV1",
            "type": "object",
            "description": "Summary line.\n\nRationale paragraph that stays in rustdoc.",
            "properties": {
                "description": { "type": "string", "description": "Single paragraph\nwrapped." },
                "page": { "$ref": "#/$defs/PageRequest" }
            },
            "$defs": {
                "PageRequest": {
                    "type": "object",
                    "description": "Bounded page.\n\nResume authorization happens first.",
                    "properties": { "page_size": { "type": "integer" } }
                }
            }
        });

        let projected = mcp_input_schema(&canonical);

        assert_eq!(
            projected,
            json!({
                "type": "object",
                "description": "Summary line.",
                "properties": {
                    "description": { "type": "string", "description": "Single paragraph\nwrapped." },
                    "page": { "$ref": "#/$defs/PageRequest" }
                },
                "$defs": {
                    "PageRequest": {
                        "type": "object",
                        "description": "Bounded page.",
                        "properties": { "page_size": { "type": "integer" } }
                    }
                }
            })
        );
    }

    #[test]
    fn bounded_union_keeps_every_tag_and_prunes_orphaned_definitions() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "value": { "$ref": "#/$defs/ValueV1" },
                "key": { "$ref": "#/$defs/Key" }
            },
            "$defs": {
                "Key": { "type": "string" },
                "Nested": { "type": "object", "properties": { "leaf": { "$ref": "#/$defs/Leaf" } } },
                "Leaf": { "type": "integer" },
                "ValueV1": {
                    "description": "Typed values.",
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {
                                "kind": { "type": "string", "const": "boolean" },
                                "value": { "type": "boolean" }
                            },
                            "required": ["kind", "value"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "kind": { "type": "string", "const": "nested" },
                                "value": { "$ref": "#/$defs/Nested" }
                            },
                            "required": ["kind", "value"]
                        }
                    ]
                }
            }
        });

        bound_tagged_union(&mut schema, "ValueV1", "Read the setting first.").expect("bounded");

        assert_eq!(
            schema["$defs"]["ValueV1"],
            json!({
                "type": "object",
                "description": "Typed values. Read the setting first.",
                "properties": {
                    "kind": { "type": "string", "enum": ["boolean", "nested"] },
                    "value": { "description": "Typed payload selected by `kind`." }
                },
                "required": ["kind", "value"],
                "additionalProperties": false
            })
        );
        assert_eq!(
            schema["$defs"]
                .as_object()
                .expect("definitions")
                .keys()
                .collect::<Vec<_>>(),
            ["Key", "ValueV1"]
        );
    }

    #[test]
    fn bounded_union_refuses_a_shape_that_is_not_an_adjacently_tagged_union() {
        let mut schema = json!({
            "type": "object",
            "properties": { "value": { "$ref": "#/$defs/ValueV1" } },
            "$defs": {
                "ValueV1": {
                    "oneOf": [
                        { "type": "object", "properties": { "boolean": { "type": "boolean" } } }
                    ]
                }
            }
        });

        assert!(bound_tagged_union(&mut schema, "ValueV1", "guidance").is_err());
        assert!(bound_tagged_union(&mut schema, "Missing", "guidance").is_err());
    }
}
