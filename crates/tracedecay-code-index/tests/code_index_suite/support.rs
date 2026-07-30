use std::fmt::Debug;

use tracedecay_code_index::chunks::content_digest;
use tracedecay_code_index::intake::{CodeIndexIntake, ReceiptBoundCodeFileV1, SanitizedCodeIntake};
use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_domain::{
    CodeGenerationId, FileOccurrenceId, LanguageDescriptorV1, LanguageId, ManifestDigest,
    RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, SnapshotFileDispositionV1, UtcMicros, ValidatedCodeFileV1,
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

pub fn validated_rust_file(source: &[u8]) -> ReceiptBoundCodeFileV1 {
    let file = SanitizedCodeFileV1 {
        file_occurrence_id: id::<FileOccurrenceId>("file.fixture"),
        logical_path: "src/lib.rs".to_owned(),
        language: Some(id::<LanguageId>("rust")),
        content_digest: content_digest(source),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let intake = SanitizedCodeIntake::new(
        registry(),
        id::<SanitizerRevision>("sanitizer.v1"),
        UtcMicros(1_000_000),
    );
    let capability = intake
        .admit(SanitizedCodeSnapshotV1 {
            repository: id::<RepositoryId>("repo.fixture"),
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
            sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.fixture")],
            content_identity: content_digest(source),
            captured_at: UtcMicros(1_000_000),
            files: vec![file.clone()],
        })
        .expect("snapshot capability");
    intake
        .bind_file(
            &capability,
            ValidatedCodeFileV1 {
                generation_id: id::<CodeGenerationId>("generation.fixture"),
                file,
                snapshot_digest: capability.snapshot().intake_digest.clone(),
                sanitized_bytes: source.to_vec(),
            },
        )
        .expect("receipt-bound rust source")
}
