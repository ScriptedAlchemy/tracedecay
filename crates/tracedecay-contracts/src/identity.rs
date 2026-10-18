//! One bounded-string identifier newtype generator shared by every application
//! contract module.
//!
//! Application identifiers are all the same value object: a non-empty, trimmed,
//! length-bounded, control-character-free `String` that validates on
//! construction and on deserialization. Only two axes ever varied between the
//! per-module copies of this generator, so both are expressed as macro arms
//! rather than as separate macros:
//!
//! * whether the newtype participates in JSON Schema generation, and
//! * whether it offers the `Display` / `TryFrom<String>` conveniences.

use crate::error::ApplicationContractError;

/// Reject identifiers that are empty, untrimmed, over `maximum_bytes`, or carry
/// control characters. `field` names the offending contract field in the error.
pub(crate) fn validate_identifier(
    value: &str,
    field: &'static str,
    maximum_bytes: usize,
) -> Result<(), ApplicationContractError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > maximum_bytes
        || value.chars().any(char::is_control)
    {
        return Err(ApplicationContractError::InvalidIdentifier { field });
    }
    Ok(())
}

/// Emit the inherent constructor, accessor, and validating `Deserialize` shared
/// by every arm of [`application_identifier!`].
macro_rules! application_identifier_body {
    ($name:ident, $field:literal, $maximum_bytes:expr) => {
        impl $name {
            /// Validate and construct the identifier. It must be non-empty,
            /// trimmed, bounded, and free of control characters.
            pub fn new(value: impl Into<String>) -> Result<Self, ApplicationContractError> {
                let value = value.into();
                $crate::identity::validate_identifier(&value, $field, $maximum_bytes)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

/// Emit the `Display` / `TryFrom<String>` conveniences.
macro_rules! application_identifier_conversions {
    ($name:ident) => {
        impl TryFrom<String> for $name {
            type Error = ApplicationContractError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

/// Declare one or more validated, bounded-string identifier newtypes.
///
/// ```ignore
/// application_identifier!(
///     /// Doc comments and other attributes pass through.
///     RequestId => ("request id", 512),
/// );
/// application_identifier!(@no_schema OpaqueCursor => ("opaque cursor", 4_096));
/// application_identifier!(@no_conversions StoreKeyV1 => ("storage store key", 256));
/// ```
///
/// The invoking module must have `ApplicationContractError`, `Serialize`,
/// `Deserialize`, and `Deserializer` in scope, plus `JsonSchema` unless
/// `@no_schema` is used and `fmt` unless `@no_conversions` is used.
macro_rules! application_identifier {
    // Serialization-only identifiers that are deliberately absent from the
    // generated JSON Schema surface.
    (@no_schema $($(#[$meta:meta])* $name:ident => ($field:literal, $maximum_bytes:expr)),+ $(,)?) => {$(
        $(#[$meta])*
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::identity::application_identifier_body!($name, $field, $maximum_bytes);
        $crate::identity::application_identifier_conversions!($name);
    )+};

    // Identifiers that intentionally expose no `Display`/`TryFrom` shortcut, so
    // callers must go through the validating constructor.
    (@no_conversions $($(#[$meta:meta])* $name:ident => ($field:literal, $maximum_bytes:expr)),+ $(,)?) => {$(
        $(#[$meta])*
        #[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::identity::application_identifier_body!($name, $field, $maximum_bytes);
    )+};

    // Default: schema-visible with the full conversion surface.
    ($($(#[$meta:meta])* $name:ident => ($field:literal, $maximum_bytes:expr)),+ $(,)?) => {$(
        $(#[$meta])*
        #[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::identity::application_identifier_body!($name, $field, $maximum_bytes);
        $crate::identity::application_identifier_conversions!($name);
    )+};
}

pub(crate) use application_identifier;
pub(crate) use application_identifier_body;
pub(crate) use application_identifier_conversions;
