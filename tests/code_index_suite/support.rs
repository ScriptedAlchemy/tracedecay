use std::fmt::Debug;

use tracedecay::code_index::chunks::content_digest;
use tracedecay::code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_domain::{
    CodeGenerationId, FileOccurrenceId, LanguageDescriptorV1, LanguageId, ManifestDigest,
    SanitizedCodeFileV1, SnapshotFileDispositionV1, ValidatedCodeFileV1,
};

pub const RUST_SOURCE: &str = "//! Module documentation.\n\nuse std::collections::HashMap;\n\n/// Increment a value.\npub fn alpha(value: u32) -> u32 {\n    value + 1\n}\n\npub struct Holder {\n    map: HashMap<u32, u32>,\n}\n\nimpl Holder {\n    pub fn get(&self, key: u32) -> Option<u32> {\n        self.map.get(&key).copied()\n    }\n}\n\n// trailing window text\n";

pub fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

pub fn digest(byte: char) -> ManifestDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

pub fn registry() -> StaticLanguageRegistry {
    StaticLanguageRegistry::new()
}

pub fn rust_descriptor() -> LanguageDescriptorV1 {
    registry()
        .descriptor(&id::<LanguageId>("rust"))
        .expect("rust descriptor")
        .clone()
}

pub fn validated_rust_file(source: &[u8]) -> ValidatedCodeFileV1 {
    ValidatedCodeFileV1 {
        generation_id: id::<CodeGenerationId>("generation.fixture"),
        file: SanitizedCodeFileV1 {
            file_occurrence_id: id::<FileOccurrenceId>("file.fixture"),
            logical_path: "src/lib.rs".to_owned(),
            language: Some(id::<LanguageId>("rust")),
            content_digest: content_digest(source),
            disposition: SnapshotFileDispositionV1::Present,
        },
        snapshot_digest: digest('a'),
        sanitized_bytes: source.to_vec(),
    }
}
