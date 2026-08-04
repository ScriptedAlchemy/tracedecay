use tracedecay_sqlite_parity_protocol::{
    Command, EffectiveJournalMode, IntegrityCheck, JournalModeMetadata, JournalModeNormalization,
    Output, SourceHeaderJournalMode, SourceJournalMode,
};

use super::support::{execute, fixture};

#[test]
fn metadata_schema_and_integrity() {
    let fixture = fixture();
    let Output::Metadata(metadata) = execute(&fixture.path, Command::Metadata) else {
        panic!("metadata output expected");
    };
    assert!(metadata.query_only && metadata.immutable);
    assert!(
        metadata
            .sqlite_version
            .split('.')
            .all(|part| part.parse::<u32>().is_ok())
    );
    assert!(
        metadata
            .compile_options
            .windows(2)
            .all(|pair| pair[0] <= pair[1])
    );
    assert!(
        metadata
            .compile_options
            .iter()
            .any(|option| option == "ENABLE_FTS5")
    );

    let Output::Schema(schema) = execute(&fixture.path, Command::Schema) else {
        panic!("schema output expected");
    };
    assert_eq!(schema.user_version, 7);
    assert!(
        schema
            .objects
            .iter()
            .any(|object| object.name == "observations")
    );
    assert!(matches!(
        execute(&fixture.path, Command::ForeignKeys),
        Output::ForeignKeys { .. }
    ));
    assert_eq!(
        execute(&fixture.path, Command::PageSize),
        Output::PageSize { bytes: 4096 }
    );
    assert_eq!(
        execute(&fixture.path, Command::JournalMode),
        Output::JournalMode(JournalModeMetadata {
            source_header: SourceHeaderJournalMode {
                read_version: 1,
                write_version: 1,
                mode: SourceJournalMode::Rollback,
            },
            mode: EffectiveJournalMode::Delete,
            immutable_effective_mode: EffectiveJournalMode::Delete,
            normalization: JournalModeNormalization::RollbackSourceImmutableDelete,
        })
    );
    let Output::Integrity(report) = execute(
        &fixture.path,
        Command::Integrity {
            check: IntegrityCheck::Full,
        },
    ) else {
        panic!("integrity output expected");
    };
    assert_eq!(report.findings, ["ok"]);
}
