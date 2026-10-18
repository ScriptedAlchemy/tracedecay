use serde_json::Value;
use tracedecay_domain::CanonicalMessageRoleV1;

/// Whether a native message `content` value carries nothing renderable:
/// null, blank text, or an empty collection. Numbers and booleans count as
/// content.
pub fn content_is_empty(content: &Value) -> bool {
    match content {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Cursor and Codex share this map. `developer` is their system alias; every
/// other label, including a missing role, uses the canonical wire parser.
pub(crate) fn canonical_message_role(role: Option<&str>) -> CanonicalMessageRoleV1 {
    match role {
        Some("developer") => CanonicalMessageRoleV1::System,
        Some(label) => CanonicalMessageRoleV1::from_wire_label(label),
        None => CanonicalMessageRoleV1::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::CanonicalMessageRoleV1;

    use super::canonical_message_role;

    #[test]
    fn developer_is_system_and_other_labels_use_the_canonical_map() {
        assert_eq!(
            canonical_message_role(Some("developer")),
            CanonicalMessageRoleV1::System
        );
        assert_eq!(
            canonical_message_role(Some("user")),
            CanonicalMessageRoleV1::User
        );
        assert_eq!(
            canonical_message_role(Some("assistant")),
            CanonicalMessageRoleV1::Assistant
        );
        assert_eq!(
            canonical_message_role(Some("system")),
            CanonicalMessageRoleV1::System
        );
        assert_eq!(
            canonical_message_role(Some("tool")),
            CanonicalMessageRoleV1::Tool
        );
        assert_eq!(
            canonical_message_role(Some("model")),
            CanonicalMessageRoleV1::Unknown
        );
        assert_eq!(
            canonical_message_role(None),
            CanonicalMessageRoleV1::Unknown
        );
    }
}
