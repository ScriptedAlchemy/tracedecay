mod common;

use tracedecay_application::{
    DiagnosticProviderDescriptor, DiagnosticProviderIdentity, DiagnosticProviderIdentityParts,
    DiagnosticProviderResult, DiagnosticProviderState, ProviderCoverage, ProviderDocumentIdentity,
    ProviderFreshness, ProviderOrigin, ProviderProvenance, ProviderSourceIdentity, RevisionDigest,
};
use tracedecay_domain::feedback::ProviderEvaluationStateV1;
use tracedecay_domain::{
    CodeGenerationId, ComponentVersion, ContentDigest, FileOccurrenceId, HostInstanceId,
    LanguageDescriptorRevision, LanguageId, ProviderId, SessionId, UtcMicros,
};
use tracedecay_tool_catalog::CapabilityId;

fn identity(source: ProviderSourceIdentity) -> DiagnosticProviderIdentity {
    DiagnosticProviderIdentity::new(DiagnosticProviderIdentityParts {
        scope: common::scope(),
        source,
        document: ProviderDocumentIdentity {
            file: common::id::<FileOccurrenceId>("file.fixture"),
            content_digest: common::id::<ContentDigest>(common::SHA256_A),
            document_version: Some(7),
        },
        producer: DiagnosticProviderDescriptor {
            provider: common::id::<ProviderId>("provider.fixture"),
            analyzer_revision: common::id::<ComponentVersion>("analyzer.fixture.v1"),
            language: common::id::<LanguageId>("rust"),
            language_descriptor_revision: common::id::<LanguageDescriptorRevision>(
                "language.rust.fixture.v1",
            ),
        },
        requested_capability: CapabilityId::new("capability.diagnostics.current").unwrap(),
        freshness: ProviderFreshness::current(UtcMicros(2)),
        coverage: ProviderCoverage::complete(1, 1),
        provenance: ProviderProvenance {
            origin: ProviderOrigin::ConfiguredAnalyzer,
            anchor: None,
        },
        configuration: RevisionDigest {
            revision: common::id::<ComponentVersion>("configuration.fixture.v1"),
            digest: common::digest(common::SHA256_A),
        },
        policy: common::authority(&common::context(&common::operation()))
            .policy
            .clone(),
    })
    .unwrap()
}

#[test]
fn provider_identity_keeps_clean_and_session_overlay_results_distinct() {
    let clean = identity(ProviderSourceIdentity::CleanGeneration {
        generation: common::id::<CodeGenerationId>("generation.v1.aaaaaaaa.00000001"),
    });
    let overlay = identity(ProviderSourceIdentity::SessionOverlay {
        session_id: common::id::<SessionId>("session.fixture"),
        client_id: common::id::<HostInstanceId>("client.fixture"),
        document_version: 7,
        overlay_digest: common::digest(common::SHA256_B),
    });

    assert_ne!(
        clean.compute_digest().unwrap(),
        overlay.compute_digest().unwrap()
    );
    assert!(!clean.is_overlay());
    assert!(overlay.is_overlay());
}

#[test]
fn provider_results_preserve_complete_coverage_and_feedback_state() {
    let clean = identity(ProviderSourceIdentity::CleanGeneration {
        generation: common::id::<CodeGenerationId>("generation.v1.aaaaaaaa.00000001"),
    });

    assert!(
        DiagnosticProviderResult::<Vec<tracedecay_domain::GenerationDiagnosticV1>>::new(
            clean.clone(),
            DiagnosticProviderState::SupportedComplete,
            None,
        )
        .is_err()
    );
    let result = DiagnosticProviderResult::new(
        clean,
        DiagnosticProviderState::SupportedComplete,
        Some(Vec::<tracedecay_domain::GenerationDiagnosticV1>::new()),
    )
    .unwrap();

    assert_eq!(
        result.state.feedback_state(),
        ProviderEvaluationStateV1::SupportedCompletedComplete
    );
}
