//! The one canonical-text predicate shared by every bounded identity, label,
//! and free-text value in the domain and store contracts.
//!
//! A canonical string is non-empty, already trimmed, and free of control
//! characters. Callers add their own byte bound and, more importantly, their
//! own rejection mapping: some contracts distinguish an empty value from a
//! merely non-canonical one, others collapse both into a single rejection.
//! Only the predicate is shared — never the error, so no contract's
//! accept/reject reporting changes by reusing it.

use crate::research::DomainError;

/// Byte bound shared by canonical identities and labels across the contracts.
pub const CANONICAL_TEXT_MAX_BYTES: usize = 512;

/// Non-empty, already trimmed, and free of control characters.
///
/// Unbounded on purpose: contracts that carry a byte bound state it through
/// [`is_canonical_text_within`] so the bound stays visible at the call site.
#[must_use]
pub fn is_canonical_text(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control)
}

/// [`is_canonical_text`] plus an explicit byte bound.
#[must_use]
pub fn is_canonical_text_within(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes && is_canonical_text(value)
}

/// Canonical bounded string that reports an empty value distinctly from a
/// non-canonical one.
///
/// This is the shared body behind the identically-specified per-module
/// validators (canonical identities, configuration labels, feedback labels).
pub(crate) fn validate_canonical_string(
    value: &str,
    field: &'static str,
) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Empty { field });
    }
    if !is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES) {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

/// Canonical bounded string that reports every rejection, empty included, as
/// non-canonical.
pub(crate) fn validate_canonical_identity(
    value: &str,
    field: &'static str,
) -> Result<(), DomainError> {
    if is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES) {
        Ok(())
    } else {
        Err(DomainError::NonCanonical { field })
    }
}

/// Declare `#[serde(transparent)]` string-identity newtypes that share one
/// surface: `new`, `as_str`, `validate`, validating `Deserialize`,
/// `TryFrom<String>`, and `Display`.
///
/// Every identity family in this crate emitted exactly this code and differed
/// only in three axes, so those are the parameters: whether the type carries a
/// JSON schema, which error the family rejects with, and which validator it
/// runs. The rejection `field` is the type name unless the family spells out a
/// label with `=>`; both forms exist because both are already on the wire in
/// error messages.
///
/// The expansion expects `Serialize`, `Deserialize`, `Deserializer`, `fmt`,
/// and (for `schema`) `JsonSchema` in scope at the invocation site, matching
/// the per-module macros this replaces.
macro_rules! validated_string_newtype {
    (@body $name:ident, $error:ty, $validate:path, $field:expr) => {
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, $error> {
                let value = value.into();
                $validate(&value, $field)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), $error> {
                $validate(&self.0, $field)
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

        impl TryFrom<String> for $name {
            type Error = $error;

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

    (schema, $error:ty, $validate:path; $($name:ident => $field:literal),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed canonical identity: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::canonical_text::validated_string_newtype!(@body $name, $error, $validate, $field);
    )+};

    (plain, $error:ty, $validate:path; $($name:ident => $field:literal),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed canonical identity: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::canonical_text::validated_string_newtype!(@body $name, $error, $validate, $field);
    )+};

    (schema, $error:ty, $validate:path; $($name:ident),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed canonical identity: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::canonical_text::validated_string_newtype!(@body $name, $error, $validate, stringify!($name));
    )+};

    (plain, $error:ty, $validate:path; $($name:ident),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed canonical identity: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::canonical_text::validated_string_newtype!(@body $name, $error, $validate, stringify!($name));
    )+};
}

pub(crate) use validated_string_newtype;

#[cfg(test)]
mod tests {
    use super::*;

    /// The predicate is the exact conjunction the per-module copies spelled
    /// out inline, so replacing them cannot move any accept/reject boundary.
    #[test]
    fn predicate_matches_the_inlined_conjunction() {
        for value in [
            "",
            " ",
            "ok",
            " lead",
            "trail ",
            "in\tner",
            "in\nner",
            "\u{7f}",
            "unicode-é",
            &"x".repeat(512),
            &"x".repeat(513),
        ] {
            let inlined = !(value.is_empty()
                || value.trim() != value
                || value.len() > CANONICAL_TEXT_MAX_BYTES
                || value.chars().any(char::is_control));
            assert_eq!(
                is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES),
                inlined,
                "canonical predicate diverged for {value:?}"
            );
        }
    }

    /// `trim().is_empty()` and `is_empty()` reject the same set once the
    /// already-trimmed requirement is also applied.
    #[test]
    fn blank_and_empty_reject_identically() {
        for value in ["", " ", "\t", "   \n "] {
            assert!(!is_canonical_text(value));
        }
    }

    #[test]
    fn empty_is_reported_distinctly_only_where_specified() {
        assert_eq!(
            validate_canonical_string("", "field"),
            Err(DomainError::Empty { field: "field" })
        );
        assert_eq!(
            validate_canonical_identity("", "field"),
            Err(DomainError::NonCanonical { field: "field" })
        );
    }
}
