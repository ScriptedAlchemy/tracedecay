use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Maximum UTF-8 byte length for a catalog-owned stable identifier.
pub const MAX_CATALOG_IDENTIFIER_BYTES: usize = 192;

/// Rejection returned when a stable catalog identifier is not canonical.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum IdentifierError {
    #[error("{kind} must not be empty")]
    Empty { kind: &'static str },
    #[error("{kind} must use lower-case ASCII identifier syntax")]
    NonCanonical { kind: &'static str },
}

pub(crate) fn validate_identifier(value: &str, kind: &'static str) -> Result<(), IdentifierError> {
    if value.is_empty() {
        return Err(IdentifierError::Empty { kind });
    }

    let bytes = value.as_bytes();
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if value.len() > MAX_CATALOG_IDENTIFIER_BYTES
        || !is_identifier_edge(first)
        || !is_identifier_edge(last)
        || bytes
            .iter()
            .copied()
            .any(|byte| !is_identifier_character(byte))
    {
        return Err(IdentifierError::NonCanonical { kind });
    }

    Ok(())
}

fn is_identifier_edge(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

fn is_identifier_character(byte: u8) -> bool {
    is_identifier_edge(byte) || matches!(byte, b'.' | b'-' | b'_')
}

macro_rules! catalog_id {
    ($($name:ident),+ $(,)?) => {
        $(
            #[doc = concat!("Stable, canonical catalog identity for `", stringify!($name), "`.")]
            #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
            #[serde(transparent)]
            pub struct $name(String);

            impl $name {
                pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
                    let value = value.into();
                    validate_identifier(&value, stringify!($name))?;
                    Ok(Self(value))
                }

                pub fn as_str(&self) -> &str {
                    &self.0
                }
            }

            impl AsRef<str> for $name {
                fn as_ref(&self) -> &str {
                    self.as_str()
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str(&self.0)
                }
            }

            impl FromStr for $name {
                type Err = IdentifierError;

                fn from_str(value: &str) -> Result<Self, Self::Err> {
                    Self::new(value)
                }
            }

            impl TryFrom<String> for $name {
                type Error = IdentifierError;

                fn try_from(value: String) -> Result<Self, Self::Error> {
                    Self::new(value)
                }
            }

            impl<'de> Deserialize<'de> for $name {
                fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
                where
                    D: Deserializer<'de>,
                {
                    Self::new(String::deserialize(deserializer)?)
                        .map_err(serde::de::Error::custom)
                }
            }
        )+
    };
}

catalog_id!(
    BindingId,
    CapabilityId,
    CodecBindingKey,
    ContributionId,
    FeatureId,
    OperationId,
    ProfileId,
    RetrieverId,
    SchemaId,
    ServiceId,
    SortContractId,
    UseCaseId,
);

/// SHA-256 digest of a versioned, canonically ordered catalog snapshot.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CatalogDigest([u8; 32]);

impl CatalogDigest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn sha256(payload: impl AsRef<[u8]>) -> Self {
        let digest: [u8; 32] = Sha256::digest(payload.as_ref()).into();
        Self(digest)
    }

    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    pub fn parse(value: &str) -> Result<Self, CatalogDigestError> {
        let Some(encoded) = value.strip_prefix("sha256:") else {
            return Err(CatalogDigestError::Malformed);
        };
        if encoded.len() != 64 {
            return Err(CatalogDigestError::Malformed);
        }

        let mut bytes = [0_u8; 32];
        for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
            let high = decode_hex(pair[0]).ok_or(CatalogDigestError::Malformed)?;
            let low = decode_hex(pair[1]).ok_or(CatalogDigestError::Malformed)?;
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

fn decode_hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

impl fmt::Display for CatalogDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("sha256:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for CatalogDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CatalogDigest")
            .field(&self.to_string())
            .finish()
    }
}

impl FromStr for CatalogDigest {
    type Err = CatalogDigestError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for CatalogDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CatalogDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Rejection returned for a non-canonical catalog digest string.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("catalog digest must be a lowercase sha256:<64 hex characters> value")]
pub enum CatalogDigestError {
    Malformed,
}
