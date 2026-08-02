use std::path::{Path, PathBuf};

use tracedecay_store::StoreShardScopeV1;

const GRAPH_MEMORY_APPLICATION_ID_V2: u32 = u32::from_be_bytes(*b"TDG2");
const REGISTERED_APPLICATION_ID_V2: u32 = u32::from_be_bytes(*b"TDR2");
const FINAL_SCHEMA_VERSION_V2: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreSchemaKindV2 {
    GraphMemory,
    Registered,
}

impl StoreSchemaKindV2 {
    pub const fn for_scope(scope: &StoreShardScopeV1) -> Self {
        match scope {
            StoreShardScopeV1::Code { .. }
            | StoreShardScopeV1::ProfileMemory
            | StoreShardScopeV1::Project { .. } => Self::GraphMemory,
            StoreShardScopeV1::Profile
            | StoreShardScopeV1::ProfileSessions
            | StoreShardScopeV1::ProjectSessions { .. } => Self::Registered,
        }
    }

    pub const fn application_id(self) -> u32 {
        match self {
            Self::GraphMemory => GRAPH_MEMORY_APPLICATION_ID_V2,
            Self::Registered => REGISTERED_APPLICATION_ID_V2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreSchemaContractV2 {
    kind: StoreSchemaKindV2,
    application_id: u32,
    user_version: u32,
    catalog_fingerprint: String,
}

impl StoreSchemaContractV2 {
    pub fn new(
        kind: StoreSchemaKindV2,
        catalog_fingerprint: impl Into<String>,
    ) -> Result<Self, StoreSchemaContractErrorV2> {
        let catalog_fingerprint = catalog_fingerprint.into();
        if !is_sha256_hex(&catalog_fingerprint) {
            return Err(StoreSchemaContractErrorV2::InvalidCatalogFingerprint);
        }
        Ok(Self {
            kind,
            application_id: kind.application_id(),
            user_version: FINAL_SCHEMA_VERSION_V2,
            catalog_fingerprint,
        })
    }

    pub const fn kind(&self) -> StoreSchemaKindV2 {
        self.kind
    }

    pub const fn application_id(&self) -> u32 {
        self.application_id
    }

    pub const fn user_version(&self) -> u32 {
        self.user_version
    }

    pub fn catalog_fingerprint(&self) -> &str {
        &self.catalog_fingerprint
    }

    pub fn classify(
        &self,
        path: &Path,
        observed: ObservedStoreSchemaV2,
    ) -> Result<ExactStoreSchemaV2, ResetRequiredV2> {
        let reason = if observed.application_id == 0
            && observed.user_version == 0
            && observed.catalog_fingerprint.is_none()
        {
            Some(StoreSchemaResetReasonV2::Empty)
        } else if observed.application_id != self.application_id {
            Some(StoreSchemaResetReasonV2::WrongKind)
        } else if observed.user_version < self.user_version {
            Some(StoreSchemaResetReasonV2::Older)
        } else if observed.user_version > self.user_version {
            Some(StoreSchemaResetReasonV2::Newer)
        } else if observed.catalog_fingerprint.as_deref() != Some(&self.catalog_fingerprint) {
            Some(StoreSchemaResetReasonV2::CatalogMismatch)
        } else {
            None
        };

        match reason {
            Some(reason) => Err(ResetRequiredV2 {
                path: path.to_path_buf(),
                expected: self.clone(),
                observed,
                reason,
            }),
            None => Ok(ExactStoreSchemaV2 {
                contract: self.clone(),
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreSchemaContractErrorV2 {
    InvalidCatalogFingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedStoreSchemaV2 {
    pub application_id: u32,
    pub user_version: u32,
    pub catalog_fingerprint: Option<String>,
}

impl ObservedStoreSchemaV2 {
    pub const fn empty() -> Self {
        Self {
            application_id: 0,
            user_version: 0,
            catalog_fingerprint: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreSchemaResetReasonV2 {
    Empty,
    WrongKind,
    Older,
    Newer,
    CatalogMismatch,
    Unreadable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResetRequiredV2 {
    path: PathBuf,
    expected: StoreSchemaContractV2,
    observed: ObservedStoreSchemaV2,
    reason: StoreSchemaResetReasonV2,
}

impl ResetRequiredV2 {
    pub fn unreadable(path: impl Into<PathBuf>, expected: StoreSchemaContractV2) -> Self {
        Self {
            path: path.into(),
            expected,
            observed: ObservedStoreSchemaV2::empty(),
            reason: StoreSchemaResetReasonV2::Unreadable,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn expected(&self) -> &StoreSchemaContractV2 {
        &self.expected
    }

    pub fn observed(&self) -> &ObservedStoreSchemaV2 {
        &self.observed
    }

    pub const fn reason(&self) -> StoreSchemaResetReasonV2 {
        self.reason
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactStoreSchemaV2 {
    contract: StoreSchemaContractV2,
}

impl ExactStoreSchemaV2 {
    pub fn contract(&self) -> &StoreSchemaContractV2 {
        &self.contract
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn contract(kind: StoreSchemaKindV2) -> StoreSchemaContractV2 {
        StoreSchemaContractV2::new(kind, FINGERPRINT).unwrap()
    }

    fn exact_observation(contract: &StoreSchemaContractV2) -> ObservedStoreSchemaV2 {
        ObservedStoreSchemaV2 {
            application_id: contract.application_id(),
            user_version: contract.user_version(),
            catalog_fingerprint: Some(contract.catalog_fingerprint().to_owned()),
        }
    }

    #[test]
    fn scope_selects_one_physical_schema_family() {
        assert_eq!(
            StoreSchemaKindV2::for_scope(&StoreShardScopeV1::Profile),
            StoreSchemaKindV2::Registered
        );
        assert_eq!(
            StoreSchemaKindV2::for_scope(&StoreShardScopeV1::ProfileMemory),
            StoreSchemaKindV2::GraphMemory
        );
    }

    #[test]
    fn only_exact_final_schema_mints_proof() {
        let contract = contract(StoreSchemaKindV2::Registered);
        let proof = contract
            .classify(
                Path::new("/isolated/global.db"),
                exact_observation(&contract),
            )
            .unwrap();
        assert_eq!(proof.contract(), &contract);
    }

    #[test]
    fn every_incompatible_existing_shape_requires_reset() {
        let contract = contract(StoreSchemaKindV2::Registered);
        let cases = [
            (
                ObservedStoreSchemaV2::empty(),
                StoreSchemaResetReasonV2::Empty,
            ),
            (
                ObservedStoreSchemaV2 {
                    application_id: StoreSchemaKindV2::GraphMemory.application_id(),
                    ..exact_observation(&contract)
                },
                StoreSchemaResetReasonV2::WrongKind,
            ),
            (
                ObservedStoreSchemaV2 {
                    user_version: 0,
                    ..exact_observation(&contract)
                },
                StoreSchemaResetReasonV2::Older,
            ),
            (
                ObservedStoreSchemaV2 {
                    user_version: contract.user_version() + 1,
                    ..exact_observation(&contract)
                },
                StoreSchemaResetReasonV2::Newer,
            ),
            (
                ObservedStoreSchemaV2 {
                    catalog_fingerprint: Some("f".repeat(64)),
                    ..exact_observation(&contract)
                },
                StoreSchemaResetReasonV2::CatalogMismatch,
            ),
        ];

        for (observed, expected_reason) in cases {
            let error = contract
                .classify(Path::new("/isolated/store.db"), observed)
                .unwrap_err();
            assert_eq!(error.reason(), expected_reason);
            assert_eq!(error.path(), Path::new("/isolated/store.db"));
        }
    }

    #[test]
    fn catalog_fingerprint_must_be_canonical_sha256_hex() {
        assert_eq!(
            StoreSchemaContractV2::new(StoreSchemaKindV2::Registered, "ABC"),
            Err(StoreSchemaContractErrorV2::InvalidCatalogFingerprint)
        );
    }
}
