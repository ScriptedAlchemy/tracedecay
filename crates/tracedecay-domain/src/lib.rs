//! Pure, versioned domain contracts.
//!
//! This crate contains values and validation only. It performs no I/O,
//! persistence, query execution, policy evaluation, host integration, or async work.

pub mod automation;
pub mod canonical_text;
pub mod code_intelligence;
pub mod configuration;
pub mod diagnostics;
pub mod errors;
pub mod external_source;
pub mod feedback;
pub mod framed_log;
pub mod git;
pub mod integration;
pub mod memory;
pub mod multi_root;
pub mod observability;
pub mod observation;
pub mod remote;
pub mod repository;
pub mod research;
pub mod resource_policy;
pub mod retrieval;
pub mod session;
pub mod session_derived;
pub mod source_path_policy;
pub mod work;
pub mod work_duplicate_adjudication;
pub mod work_execution_snapshot;
pub mod work_placement;
pub mod work_product;
pub mod work_product_event;
pub mod work_product_projection;
pub mod work_read;
pub mod work_routing;
pub mod work_run_control;
pub mod work_runtime;
pub mod workflow;
pub mod workflow_fan_out_census;
pub mod workflow_receipt;
pub mod workflow_run;

pub use automation::{SESSION_EVIDENCE_BUDGET_EXHAUSTED, SESSION_EVIDENCE_BUDGET_SUPPRESSED};
pub use canonical_text::sha256_hex_suffix;
pub use code_intelligence::{
    AdaptiveRecallDepthPolicyV1, AdaptiveRecallStepV1, AdaptiveRecallStopV1,
    AdmittedEmbeddingProjectionKeyV1, BoundedSanitizedText, CanonicalRelationEdgeV1,
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkLogicalIdentityV1, ChunkerRevision,
    CodeChunkProjectionReceiptV1, CodeGenerationId, CodeGenerationManifestV1,
    CodeGenerationSourceCommitmentsV1, CodeIndexCapabilityManifestV1, CodeSearchChunkAnchorV1,
    CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1, ComplexityAnalysisV1,
    ContentDigest, CoverageSummaryV1, EMBEDDING_PROJECTION_SCHEMA_V1, Edge, EdgeAuthorityV1,
    EdgeKind, EmbeddingDeviceClassV1, EmbeddingDocumentCompositionV1, EmbeddingMetricV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
    EmbeddingTruncationSideV1, EphemeralSanitizedQueryViewV1, ExactTechnicalTermKindV1,
    ExactTechnicalTermV1, ExpandoBehaviorV1, ExtractionAdmittedChunkV1, ExtractionResult,
    ExtractorRevision, FileIdentityDigest, FileOccurrenceId, GenerationDiagnosticAttachmentV1,
    GenerationPlannerIdV1, GenerationSealV1, GenerationTestAttributionV1, GrammarRevision,
    GraphStats, LanguageCapabilitySetV1, LanguageDescriptorRevision, LanguageDescriptorV1,
    LanguageId, LanguageRegistryRevision, MAX_CHUNK_TEXT_BYTES, MAX_EPHEMERAL_QUERY_VIEW_BYTES,
    Node, NodeKind, PROJECTION_PUBLICATION_SEPARATOR, PolicyRevisionId, ProjectionBatchReceiptV1,
    ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1, ProjectionOperationV1,
    ProjectionOutcomeV1, ProjectionReplayReasonV1, QueryNormalizationRevision, RelationEdgeKindV1,
    SEMANTIC_ANN_RECALL_POLICY_V1, SEMANTIC_SEARCH_INDEX_SCHEMA_V1, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SanitizerRevision, SemanticSearchIndexKeyV1,
    SemanticSearchIndexKindV1, SemanticSearchIndexProfileV1, SensitivityDecision,
    SensitivityLevelV1, SnapshotFileDispositionV1, SourceSpan, SymbolIdentityDigest,
    SymbolOccurrenceId, TestAttributionEvidenceClassV1, UnresolvedRef, ValidatedCodeFileV1,
    VectorGenerationIdV1, Visibility, classify_technical_token, code_source_full_replay_digest,
    exact_search_canonical, generate_node_id, generate_node_id_at, is_cli_flag_token,
    is_commit_hash, is_commit_identifier_token, is_compiler_error_code_token,
    is_configuration_key_token, is_identifier_token, is_path_shape, is_path_token,
    is_qualified_name_token, is_runtime_error_code_token, is_technical_token_char,
    is_tool_name_token, projection_batch_publication_digest, repository_path_matches_scope,
    semantic_vector_output_digest, split_subtokens, technical_tokens, validate_code_logical_path,
};
pub use configuration::{
    ACCESS_RULES_SETTING_KEY, ANALYZER_SETTINGS_SETTING_KEY, AUTOMATION_SETTINGS_SETTING_KEY,
    AccessRuleId, AnalyzerEnvironmentVariable, AnalyzerExecutableId, AnalyzerExecutableReferenceV1,
    AnalyzerLanguageId, AnalyzerLanguageSelectionV1, AnalyzerPrivacyClassV1,
    AnalyzerResourceLimitsV1, AnalyzerRestartPolicyV1, AnalyzerSettingsV1,
    AnalyzerStructuredValueV1, AuthorityRef, AutomaticWorktreeGcV1, AutomationBackendV1,
    AutomationHostModeV1, AutomationSettingsV1, AutomationTaskSetV1, AutomationTaskSettingsV1,
    BranchCollisionPolicyV1, BranchNameComponentV1, BranchNameSeparatorV1, BranchNamingPolicyV1,
    BranchTopologyKindV1, BranchTopologyPolicyV1, CONFIGURATION_SETTING_KEYS_V1,
    CONTEXT_SCOUT_SETTINGS_SETTING_KEY, CandidateDispositionV1, CanonicalGitRefNameV1,
    CanonicalGitRefPrefix, CapabilityResolutionContextV1, ChangePlanId,
    CodeIndexWorkerLimitingReasonV1, CodeIndexWorkerSelectionV1, CodeIndexWorkerStatusV1,
    ConfigurationAuditEvent, ConfigurationAuditEventId, ConfigurationAuditEventKindV1,
    ConfigurationCandidateV1, ConfigurationGrantId, ConfigurationGrantReceiptId,
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationLayerKindV1,
    ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
    ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, ConfigurationReceiptId,
    ConfigurationRevisionId, ConfigurationSettlementAuthorityV1, ConfigurationSnapshotId,
    ConfigurationSnapshotV1, ConfigurationValueKindV1, ConfigurationValueV1,
    ContextScoutConfigurationLimitsV1, ContextScoutConfigurationModeV1,
    ContextScoutConfigurationStateV1, ContextScoutConfiguredModelPathV1, ContextScoutSettingsV1,
    CredentialReferenceId, CrossMergeModeV1, CrossMergePolicyV1, DIAGNOSTICS_PREWARM_SETTING_KEY,
    DeprecationStateV1, GitHubStackedPullRequestPolicyV1, HistoryRewritePolicyV1,
    INDEX_EXCLUDE_SETTING_KEY, INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY, INDEX_GIT_IGNORE_SETTING_KEY,
    INDEX_INCLUDE_SETTING_KEY, INDEX_MAX_FILE_SIZE_SETTING_KEY,
    INDEX_NATIVE_GRAPH_ACTIVATION_SETTING_KEY, INDEX_TRACK_CALL_SITES_SETTING_KEY,
    MAX_WORK_EXPERTISE_CONSENT_LIFETIME_MICROS_V1, PROJECT_WORK_EXPERTISE_CONSENT_SETTING_KEY,
    ProtectedApplyRequest, ProtectedChange, ProtectedChangePlan, ProtectedChangeSnapshotError,
    ProtectedRefDispositionV1, ProtectedRefRuleV1, ProtectedRefSelectorV1, QueryCollectionId,
    RedactedConfigurationChangeV1, RepositoryPlacementScopeV1, RequiredCheckExpectationV1,
    RequiredCheckV1, RestartRequirementV1, RestrictiveCapabilityResolutionV1, ReviewRequirementV1,
    ReviewTopologyKindV1, ReviewTopologyPolicyV1, RollbackModeV1, RuleEffect,
    SEMANTIC_RUNTIME_SETTING_KEY, SOURCE_BINDINGS_SETTING_KEY, SYNC_AUTO_INIT_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY, SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
    SYNC_AUTO_WATCH_SETTING_KEY, SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY,
    SYNC_BRANCH_GC_DAYS_SETTING_KEY, SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY,
    SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY, SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY,
    SYNC_READ_COOLDOWN_SECS_SETTING_KEY, SYNC_READ_REFRESH_SETTING_KEY,
    SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY, SYNC_SESSION_START_SYNC_SETTING_KEY,
    SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY, SYNC_WATCH_LINKED_WORKTREES_SETTING_KEY,
    SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY, SYNC_WATCH_MAX_PROJECTS_SETTING_KEY, ScopeAccessRule,
    ScopeAccessSubjectV1, ScopeControlOperationV1, ScopeSourceBinding,
    SensitiveFilesystemLocatorV1, SettingDefinitionV1, SettingKey, SettingScopeV1,
    SettingSensitivityV1, SourceBindingId, SourceKindV1, TELEMETRY_TIMINGS_SETTING_KEY,
    TopologyConcurrencyPolicyV1, TopologyEscalationPolicyV1, TopologyGatePolicyV1,
    TopologyNotificationLevelV1, TopologyPolicyDigestV1, USER_CODE_INDEX_WORKERS_SETTING_KEY,
    USER_EXTRACTION_TIMEOUT_SECS_SETTING_KEY, USER_UPLOAD_ENABLED_SETTING_KEY,
    USER_WATCHER_DEBOUNCE_MS_SETTING_KEY, USER_WORK_EXPERTISE_CONSENT_SETTING_KEY, UserProfileId,
    WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WORK_TOPOLOGY_POLICY_SETTING_KEY,
    WorkExecutableBindingV1, WorkExecutableCapabilityV1, WorkExpertiseCategoryV1,
    WorkExpertiseConsentV1, WorkTopologyPolicyV1, WorktreeCleanlinessRequirementV1,
    WorktreePlacementModeV1, WorktreePlacementRootId, WorktreeRetentionPolicyV1,
    WorktreeRootPolicyV1, resolve_restrictive_capabilities, safe_work_topology_policy_v1,
};
pub use diagnostics::{
    DiagnosticEvidenceClassV1, DiagnosticProducerKindV1, DiagnosticProvenanceV1,
    DiagnosticRecordStateV1, DiagnosticSeverityV1, GenerationDiagnosticV1, MAX_DIAGNOSTIC_CODE_LEN,
    MAX_DIAGNOSTIC_MESSAGE_BYTES,
};
pub use errors::{AutomationErrorMessage, Result, SqliteDriverError, TraceDecayError};
pub use external_source::{
    MAX_SOURCE_PARTITIONS_V1, SourceAcquisitionCapabilitiesV1, SourceAcquisitionContractV1,
    SourceAggregateFrontierV1, SourceBindingIdentityV1, SourceBindingOwnerV1, SourceBindingV1,
    SourceCaptureModeV1, SourceContentStateV1, SourceCoverageV1, SourceCursorV1,
    SourceDefinitionV1, SourceDeletionSemanticsV1, SourceEnvelopeKindV1,
    SourceEventAdmissionDispositionV1, SourceEventAdmissionReceiptV1, SourceEventKeyV1,
    SourceEventV1, SourceNativeObjectIdV1, SourceObjectObservationV1, SourceObjectRevisionV1,
    SourcePartitionFrontierV1, SourcePartitionIdV1, SourceProviderEnvelopeV1,
    SourceRefetchStrategyV1, SourceRefreshCauseV1, SourceRefreshReceiptV1,
    SourceSnapshotCompletionV1, SourceSnapshotIdV1, SourceWholeRootStageV1,
};
pub use feedback::{
    CiCallerRelationV1, CiFailureBranchEvidenceV1, CiFailureCallerEvidenceV1, CiFailureCoverageV1,
    CiFailureGenerationEvidenceV1, CiFailureKindV1, CiFailureLocalizationResultV1,
    CiFailureLocalizationStateV1, CiFailureParserIdentityV1, CiFailureRateLimitCheckpointV1,
    CiFailureRunIdentityV1, CiFailureSourceDegradationV1, CiFailureSourceFailureV1,
    CiFailureSymbolEvidenceV1, CiFailureTestEvidenceV1, CiInertRerunHintV1, CiInertRerunTargetV1,
    FeedbackActorContextV1, FeedbackAdvisoryProviderStateV1, FeedbackAuthoritativeRuntimeStateV1,
    FeedbackBaselineHorizonV1, FeedbackBaselineStateV1, FeedbackBudgetV1,
    FeedbackContentIdentityV1, FeedbackCycleId, FeedbackCycleObservationV1, FeedbackCycleRequestV1,
    FeedbackCycleResultV1, FeedbackCycleRuntimeSnapshotV1, FeedbackCycleTerminationV1,
    FeedbackDedupeClaimId, FeedbackDedupeKeyV1, FeedbackDiagnosticBaselineIdentityV1,
    FeedbackDiagnosticBaselineV1, FeedbackDiagnosticClassificationV1, FeedbackDiagnosticProducerV1,
    FeedbackDiagnosticProjectionV1, FeedbackDiagnosticV1, FeedbackDurabilityV1,
    FeedbackEvaluationInputV1, FeedbackEvaluationStageV1, FeedbackEvidencePacketV1,
    FeedbackFindingId, FeedbackFindingLifecycleV1, FeedbackFindingV1, FeedbackImpactStateV1,
    FeedbackImpactV1, FeedbackObservationKindV1, FeedbackPacketId, FeedbackResultId,
    FeedbackSavedDedupeKeyV1, FeedbackSavedEvaluationV1, FeedbackScopeV1,
    FeedbackSessionDiagnosticV1, FeedbackTargetV1, FeedbackTriggerV1, GitHubPullRequestIdV1,
    GitHubPullRequestSnapshotV1, GitHubPullRequestStateV1, GitHubReviewAuthorClassV1,
    GitHubReviewCommentIdV1, GitHubReviewCoverageV1, GitHubReviewCurrentBranchRemapV1,
    GitHubReviewCursorV1, GitHubReviewEtagV1, GitHubReviewIdV1, GitHubReviewImmutableAnchorV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1, GitHubReviewItemV1,
    GitHubReviewLifecycleV1, GitHubReviewRateLimitCheckpointV1, GitHubReviewReadCheckpointV1,
    GitHubReviewReadOperationV1, GitHubReviewRemapStateV1, GitHubReviewStateV1,
    GitHubReviewThreadIdV1, MAX_CI_FAILURE_CALLER_EVIDENCE_V1, MAX_CI_FAILURE_RERUN_HINTS_V1,
    MAX_CI_FAILURE_TEST_EVIDENCE_V1, MAX_GITHUB_PULL_REQUEST_TITLE_BYTES_V1,
    MAX_GITHUB_REVIEW_THREAD_PATH_BYTES_V1, PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1,
    ProviderEvaluationStateV1, ProximityAddressV1, ProximityBranchWorktreeIncompatibilityV1,
    ProximityContributionIdV1, ProximityContributionV1, ProximityCoverageV1, ProximityInclusionV1,
    ProximityObservationIdV1, ProximityRelationPathKindV1, ProximityRelationPathV1,
    ProximityRelationStrengthV1, ProximityRiskInputsV1, ProximityTierV1, ProximityWarningClassV1,
    ProximityWarningIdV1, derive_feedback_finding_id, derive_overlay_feedback_finding_id,
};
pub use framed_log::{CHECKSUM_BYTES, checksum, partial_tail_matches_prefix};
pub use git::{
    BranchGraphPublicationEpochV1, GIT_INDEX_COMMIT_INTENT_DIGEST_DOMAIN_V1,
    GIT_INDEX_PREVIEW_DIGEST_DOMAIN_V1, GIT_INDEX_RECEIPT_DIGEST_DOMAIN_V1,
    GIT_INDEX_SNAPSHOT_DIGEST_DOMAIN_V1, GitBlameAvailabilityV1, GitBlameLineV1,
    GitBlamePreviousV1, GitBlameV1, GitBlobExpectationV1, GitChangeKindV1, GitCommitIdentityV1,
    GitCommitMetadataV1, GitCoverageV1, GitDegradationV1, GitDiffScopeV1, GitDiffV1, GitFileDiffV1,
    GitFileModeV1, GitHeadStateV1, GitHistoryV1, GitHunkV1, GitIndexCommitIntentV1,
    GitIndexEntryExpectationV1, GitIndexIdempotencyKey, GitIndexJournalPhaseV1,
    GitIndexPreviewDispositionV1, GitIndexPreviewId, GitIndexPreviewInputV1, GitIndexPreviewV1,
    GitIndexReceiptId, GitIndexReceiptOutcomeV1, GitIndexSigningPolicyV1, GitIndexTransactionId,
    GitIndexTransactionJournalV1, GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1,
    GitIndexUnsupportedStateV1, GitObjectFormatV1, GitOidV1, GitOperationStateV1, GitStatusEntryV1,
    GitStatusV1, GitTrackedStatusV1, HUNK_REF_DIGEST_DOMAIN, HUNK_REF_SCHEMA_VERSION_V1,
    HunkDirectionV1, HunkRefV1, MAX_GIT_INDEX_PREVIEW_INPUT_HUNKS,
    MAX_GIT_INDEX_PREVIEW_INPUT_LIFETIME_MICROS, ParsedHunkHeader, RepositoryIndexSnapshotV1,
    RepositoryIndexStateV1, RepositoryStateSnapshotId, RepositoryStateSnapshotV1,
    RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1, StackSignalKindV1,
    full_hunk_selection_bitmap, parse_hunk_header,
};
pub use integration::{
    HOST_INTEGRATION_CATALOG_SCHEMA_VERSION_V1, HostActivationPolicyV1, HostAssetRenderPolicyV1,
    HostCapabilityRecordV1, HostCapabilityStateV1, HostCapabilityUnavailableReasonV1,
    HostCapabilityV1, HostCapabilityViewV1, HostComponentV1, HostDescriptorV1, HostHookMappingV1,
    HostIntegrationCatalogV1, HostIntegrationIdV1, HostKindV1, HostProjectRegistrationPathV1,
    IntegrationCapabilityV1, IntegrationCatalogError, IntegrationDaemonActionV1,
    IntegrationDaemonApiV1, IntegrationDaemonRequirementV1, IntegrationEffectClassV1,
    IntegrationPrivacyClassV1, NativeHostIdentityV1, StockHostCapabilityViewV1,
    TraceDecayProfileBindingV1, host_descriptor_v1, host_descriptors_v1,
    host_integration_catalog_v1, stock_host_capabilities,
};
pub use memory::{
    FactAssertionKindV1, FactAssertionV1, FactCategoryV1, FactCurationActionV1, FactEvidenceRefV1,
    FactEvidenceRelationV1, FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1,
    FactLineageEventV1, FactOwnerV1, FactPayloadV1, FactRelationKindV1, FactRelationProvenanceV1,
    FactRelationV1, ProjectMemoryGraphRelationKindV1,
};
pub use multi_root::{
    CollectionRevision, RootGenerationV1, RootScopeOutcomeV1, ScopeOutcome, ScopePartialReasonV1,
    ScopeSetId, ScopeSetRevision, ScopeUnavailableReasonV1, StackRevision,
};
pub use observability::{
    ActivityObservedV1, AdoptionEligibilityObservedV1, AdoptionOutcomeLinkedV1,
    AnalyticsConsentChangedV1, AnalyticsModeV1, AppropriateRelianceObservedV1,
    AutomationFunnelObservedV1, AutomationTerminalV1, BlockedCauseV1, ConflictAdjudicatorV1,
    ConflictKindV1, ConflictOutcomeV1, ConflictPredictionV1, ConflictScoreKindV1,
    ContextOutcomeObservedV1, CoverageStateV1, DeadlineClassV1, DeadlineObservedV1,
    DeadlineOutcomeV1, DeliveryChannelIdentityV1, DeliveryDropReasonV1, DeliveryEventClassV1,
    DeliverySettlementAttemptV1, DeliverySettlementCensusV1, DeliverySettlementOutcomeV1,
    DeliverySettlementV1, DeliverySurfaceFamilyV1, DuplicateEffectOutcomeV1, DuplicateEffortKindV1,
    DurationBucketV1, EffectReconciliationOutcomeV1, ExecutionPlacementV1, ExecutionTopologyKindV1,
    ExecutionTopologySampledV1, GitHubStackCapabilityObservedV1, GitHubStackCapabilityV1,
    HealthDimensionObservedV1, HealthSnapshotObservedV1, IndexObservationKindV1, IndexObservedV1,
    IndexOutcomeV1, IntegrationOperationKindV1, IntegrationOwnerReceiptV1, IntegrationPhaseV1,
    IntegrationResultV1, IntegrationScopeClassV1, IntegrationStrategyV1, IntervalStateV1,
    LatencyObservedV1, LatencyStageV1, LeakOwnerClassV1, MAX_DELIVERY_RECIPIENTS_V1,
    MAX_LOCAL_ANCHORS_V1, McpDispatchCancellationV1, McpDispatchDeadlineV1, McpDispatchObservedV1,
    McpDispatchTerminalV1, NoProgressEscalationV1, NoProgressObservedV1, ObservabilityEnvelopeV1,
    ObservabilityPayloadV1, ObservabilityRetentionClassV1, ObservabilityTerminalResultV1,
    ObservedTernaryV1, OperationActivationOutcomeV1, OperationAvailabilityV1,
    OperationPhaseTimingV1, OperationPhaseV1, OperationReadinessV1, OperationResourceObservedV1,
    OperationStageTimingV1, OperationStageV1, PerformanceDispositionV1, ProviderAttemptTerminalV1,
    ProviderReliabilityObservedV1, QuantityEvidenceClassV1, QueueDepthBucketV1,
    RejectedArgumentErrorClassV1, RejectedArgumentNameV1, RejectedArgumentObservedV1,
    RejectedArgumentSurfaceV1, RelianceDecisionV1, RelianceVerificationV1,
    RemoteCoverageObservedV1, RemoteOperationV1, RerunCauseV1, RerunSourceV1,
    RetrievalAblationObservedV1, RetrievalPlannerObservedV1, RetrievalQueryObservedV1,
    RetrievalSourceObservedV1, RetrievalSynthesisObservedV1, RetrieverObservedV1, ReviewTopologyV1,
    StackDriftKindV1, StorageObservationKindV1, StorageObservedV1, TaskCalibrationEvidenceV1,
    TaskDecisionDispositionV1, TaskIntelligenceDecisionObservedV1,
    TaskIntelligenceOutcomeObservedV1, TaskOutcomeV1, TelemetryDropObservedV1,
    WorkBlockedIntervalObservedV1, WorkConflictOutcomeLinkedV1, WorkConflictPredictionObservedV1,
    WorkDeliveryFanoutObservedV1, WorkDuplicateEffortObservedV1, WorkExecutionLeakKindV1,
    WorkExecutionLeakObservedV1, WorkExecutionLeakRecoveryV1, WorkIntegrationTransitionObservedV1,
    WorkRerunObservedV1, WorkStackDriftObservedV1, WorkTopologyBranchV1,
    WorkflowLifecycleObservedV1, WorkflowOutcomeObservedV1, WorkflowResourceObservedV1,
    WorkflowStageClassV1, validate_local_ref,
};
pub use observation::{
    CANONICAL_OBSERVATION_ENVELOPE_VERSION_V1, CanonicalBoundaryKindV1,
    CanonicalClaudeSanitizationReceiptMaterialV1, CanonicalGitEvidenceKindV1,
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationIdV1, CanonicalObservationRelationsV1,
    CanonicalReasoningVisibilityV1, CanonicalUnknownStateV1, CanonicalWorkflowEvidenceKindV1,
    CanonicalWorkflowSemanticKindV1, ClineNativeSourceTransition, ClineTranscriptStream,
    DurableClaudeObservationV1, DurableObservationV1, MAX_CANONICAL_OBSERVATION_FACTS_V1,
    MAX_OBSERVATION_RECORD_BYTES, MAX_OBSERVATION_STRUCTURE_DEPTH,
    MAX_OBSERVATION_STRUCTURE_VALUES, ObservationCollisionOutcomeV1, ObservationContractError,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationPositionalOccurrenceV1,
    ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadDigestV1, PayloadReferenceV1,
    ProviderUsageContractDimensionV1, ProviderUsageCounterSemanticsV1, ProviderUsageCountersV1,
    ProviderUsageCursorV1, ProviderUsageModelV1, ProviderUsageObservationV1, ProviderUsageReadV1,
    ProviderUsageScopeV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    classify_observation_collision, cline_native_source_successor_id,
    cline_task_native_observation_id, is_canonical_payload_revision_replay,
    prove_cline_native_source_transition,
};
pub use remote::{
    CredentialRevocationReceiptV1, CredentialRotationReceiptV1, CurrentRemoteAuthorityStateV1,
    CurrentRemoteAuthorityV1, EnrollmentCredentialRecordV1, EnrollmentCredentialStateV1,
    EnrollmentGrantV1, MAX_REMOTE_CREDENTIAL_BYTES, MIN_REMOTE_CREDENTIAL_BYTES,
    RemoteAuthorityUnavailableReasonV1, RemoteCapabilityV1, RemoteCredentialFingerprintV1,
    RemotePlacementRevisionV1, RemoteRepositoryScopeV1, RemoteWriterFenceV1,
    validate_remote_secret_length,
};
pub use repository::{
    EvidenceAvailabilityV1, GenerationBoundRepositoryProvenanceV1, RepositoryDirtyStateV1,
    RepositoryEvidenceV1, RepositoryProvenanceV1, RepositoryRemoteIdentityV1,
};
pub use research::{
    AccessPolicyDigest, ActorId, AgentInstanceId, AnchorDurabilityClass, AnchorLineageRefV2,
    AnchorLineageRefV3, AnchorOwnerBindingV1, AnchorProvenanceRelationV2, AnchorResolutionStateV2,
    AnchorSourceGenerationV2, AnchorSourceGenerationV3, ApplyReceiptAnchorRefV1, AttemptId,
    AuthorityEpoch, AuthorizedAnchorResolution, BlobId, BoundedVec, BrainId, BrainNodeId,
    BrainNodeRoleV1, BranchStackEdgeV1, BranchStackId, BranchStackNodeV1, BranchStackRevisionId,
    BranchStackRevisionV1, BranchStackSourceV1, CanonicalSourceOccurrenceSetIdV1, CapabilityId,
    CatalogGenerationId, CatalogSnapshotRefV1, CheckSnapshotAnchorRefV1, CommitId,
    ComponentVersion, Confidence, ConflictEvidenceAnchorRefV1, CoverageReportV1,
    CoverageUniverseKnowledgeV1, DataVersionDigest, DomainError, EntityId, EntityKind, EntityRef,
    EntityVersionId, EvidenceAssemblyPublicationReceiptIdV1, EvidenceClass,
    EvidenceRetentionWatermark, EvidenceSpanProjectionReceiptIdV1, FactAssertionId, FactEventId,
    FactEvidenceId, FactId, FrozenBranchStackSnapshotV1, FrozenIndependentBranchSelectionV1,
    FrozenWatermarkResolutionV1, GitHubStackCapabilitySnapshotV1, GitHubStackCapabilityStateV1,
    GitHubStackLayerSnapshotV1, GitHubStackSnapshotV1, GitTopologyAnchorTargetV1,
    GitTopologyGenerationRefV1, GitTopologySourceRoleV1, HostInstanceId,
    IntegrationReceiptAnchorRefV1, LocatorDigest, LogSafeText, ManifestDigest,
    ManifestDigestHasher, MechanicalIntegrationModeV1, MessageId, NativeAliasKindV2, NativeAliasV2,
    NativeGitObjectAnchorRefV1, NativeGitObjectKindV1, NativeIntegrationApprovalId,
    NativeIntegrationApprovalV1, NativeIntegrationDirectionV1, NativeIntegrationPhaseV1,
    NativeIntegrationPreviewDispositionV1, NativeIntegrationPreviewId, NativeIntegrationPreviewV1,
    NativeIntegrationReceiptV1, NativeIntegrationRepositorySnapshotV1,
    NativeIntegrationSelectionV1, NativeIntegrationTerminalOutcomeV1,
    NativeIntegrationTransactionId, NativeIntegrationTransactionStatusV1,
    NativeIntegrationUnavailabilityV1, NativeWorktreeCleanupCommandV1,
    NativeWorktreeCleanupOutcomeV1, NativeWorktreeCleanupPhaseV1, NativeWorktreeCleanupReceiptV1,
    NativeWorktreeCleanupTransactionV1, NonEmptyUniqueVec, ObservationId,
    OrderedGitTopologySourceV1, PayloadAccessState, PreflightPreviewAnchorRefV1,
    PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProjectionGenerationId,
    ProposalId, ProvenanceId, ProviderId, PullRequestSnapshotAnchorRefV1, ReadConsistencyV1, RefId,
    RefSnapshotAnchorRefV1, RefSnapshotKindV1, RegistryManifestDigest, RemoteCoverageV1,
    RemoteShardCoverageV1, RepositoryCaptureAnchorRefV1, RepositoryCaptureId, RepositoryId,
    ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorId, RetrievalAnchorRecord,
    RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts, RetrievalAnchorRecordV3,
    RetrievalAnchorRecordV3Parts, RetrievalAnchorTargetV2, RetrievalAnchorTargetV3,
    RetrieverContributionIdV1, ReviewSnapshotAnchorRefV1, RunId, SanitizationProofV1,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptResolverV1,
    SanitizedTextRefV1, SanitizedTextV1, ScopeResolutionId, SessionId, ShardDispositionV1, ShardId,
    ShardWatermark, SourceInstanceId, SourcePosition, SourceStoreId, StackDeliveryWatermarkId,
    StackNodeId, StackSignalId, StoreAuthorityId, TaskId, ThreadId, TimeInterval, ToolInvocationId,
    TreeId, TurnId, UseCaseId, UtcMicros, VectorWatermark, VerifiedCacheGrantSnapshotV1,
    WatermarkDriftV1, WorkArtifactId, WorkCancellationRequestId, WorkCommandId, WorkLeaseId,
    WorkProviderRouteId, WorkTopologyGenerationRefV1, WorkflowDefinitionId, WorkflowOperationRef,
    WorkflowOutputName, WorkflowStepId, WorktreeCaptureAnchorRefV1, WorktreeId,
    WorktreeInventoryEpoch, WorktreeInventorySnapshotId, canonical_json_bytes,
    canonical_json_bytes_and_sha256, canonical_json_value, canonical_sha256,
    derive_exact_observation_anchor_id, derive_exact_source_occurrence_anchor_id,
    derive_git_topology_anchor_id, validate_anchor_lineage_v3, zero_digest,
};
pub use resource_policy::host_cpu_target;
pub use retrieval::{
    AuthorizationRevision, AuthorizedRerankView, CalibrationProfileId, CandidateContribution,
    CandidateSetDigest, CodeSourceCursorBindingV1, CompactCandidate, ComponentRevision,
    CursorPayloadDigest, DiversityPolicy, DiversityPolicyId, EvaluationDecisionId, EvidenceRole,
    ExactAdmissionProof, ExactAdmissionRuleRevision, ExactAdmissionValidator, ExactClass,
    ExactFieldV1, FallbackSubpayloadDigest, FixedPointScore, FreshnessCompatibilityV1,
    FreshnessVectorDigest, FusedCandidate, FusionProfile, FusionProfileId, HydrationReceipt,
    HydrationRevision, LogicalCopyClusterId, LogicalEvidenceId, OccurrenceProvenance,
    OptionalStagePublicStatus, PrincipalId, PublicRetrieverStatus,
    QUERY_FALLBACK_SUBPAYLOAD_DIGEST_DOMAIN, QueryDigest, QueryFallbackSubpayload, QueryMac,
    RankedCandidate, RankingDecision, RankingDecisionKind, RankingRevision, RerankPolicy,
    RerankPolicyId, RetrievalBudget, RetrievalBudgetUsage, RetrievalContractError, RetrievalCursor,
    RetrievalCursorKeyId, RetrievalError, RetrievalFailure, RetrievalRequest, RetrievalScope,
    RetrievalSnapshot, RetrieverBatch, RetrieverContinuation, RetrieverCoverage, RetrieverKind,
    RetrieverOutcome, SanitizedBudgetUsage, SanitizedStageFailure, ScoreDomainCalibrationV1,
    ScoreDomainId, SemanticRetrievalContinuationV1, SessionOrThreadId, SingleRootScopeV1,
    SourceFreshness, SourceInstanceKey, SourceNamespace, SourceOccurrenceId,
    TemporalCandidateChannelV1, TemporalCandidateContributionV1, TemporalLaneEvidenceV1,
};
pub use session::{
    ByteRangeV1, ClosedUtcIntervalV1, CompactContextBundleV1, CompactContextConflictV1,
    CompactContextLineageEdgeV1, CompactContextOmissionV1, CompactContextRecordV1,
    ContextOmissionReasonV1, CopyProofV1, CursorManifestLimitKindV1, GroupingProvenanceV1,
    HydrationStateV1, LogicalCopyRecordV1, MessageOccurrenceIdV1, MessageOccurrenceRecordV1,
    ProjectionOutputOrdinalV1, RetrievalGrainV1, SESSION_TEMPORAL_CURSOR_MAX_CANONICAL_BYTES,
    SESSION_TEMPORAL_CURSOR_MAX_PARTICIPANTS, SessionAuthorityClassV1, SessionContractError,
    SessionCursorKeyIdV1, SessionCursorVersionV1, SessionEvidenceMetadataV1,
    SessionProjectionGenerationV1, SessionRefreshKeyV1, SessionRefreshOperationIdV1,
    SessionRefreshSourceTargetV1, SessionSourceCoverageAggregateStateV1,
    SessionSourceCoverageIntervalV1, SessionSourceCoverageReasonV1, SessionSourceCoverageReceiptV1,
    SessionSourceCoverageStateV1, SessionSourceCoverageV1, SessionSourceFrontierV1,
    SessionSourceIdV1, SessionSummaryIdV1, SessionSummaryRecordV1,
    SessionTemporalCoverageRequestV1, SignedCursorKeyRefV1, SummaryPublicationMetadataV1,
    SummarySourceHorizonV1, TemporalAssertionIdV1, TemporalAssertionKindV1,
    TemporalAssertionRecordV1, TemporalCoverageCountsV1, TemporalModeV1, TemporalValidityV1,
    ValidCoverageIntervalV1,
};
pub use session_derived::{
    DerivedEvidenceIdV1, DerivedEvidenceKindV1, DerivedEvidenceMemberRoleV1,
    DerivedEvidenceMemberV1, DerivedEvidenceOccurrenceRefV1, EvidenceSpanIdV1,
    SESSION_DERIVED_BURST_ALGORITHM_V1, SESSION_DERIVED_SPAN_ALGORITHM_V1,
    SESSION_DERIVED_SPAN_MAX_MEMBERS_V1, SessionDerivedEvidencePolicyV1,
    SessionDerivedEvidenceRecordV1, derive_session_evidence_from_occurrences,
};
pub use source_path_policy::{GENERATED_DIR_SEGMENTS, is_generated_dir_segment};
pub use work::{
    MAX_WORK_DEPENDENCIES, MAX_WORK_TITLE_BYTES, RuntimeEvidenceRef,
    WORK_PROJECTION_STATE_VERSION_V1, WorkAuthority, WorkContractError, WorkEvent, WorkEventKind,
    WorkProjection, WorkProjectionStateV1, WorkVersion,
};
pub use work_duplicate_adjudication::{
    MAX_WORK_DUPLICATE_REASON_BYTES_V1, WorkDuplicateAdjudicationCommandV1,
    WorkDuplicateAdjudicationContractErrorV1, WorkDuplicateAdjudicationEvidenceV1,
    WorkDuplicateAdjudicationQuantitiesV1, WorkDuplicateAdjudicationReceiptV1,
    WorkDuplicateAdjudicationRevisionV1,
};
pub use work_execution_snapshot::{
    WorkApprovalPolicy, WorkEgressPolicy, WorkExecutableReference, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFilesystemPolicy,
    WorkProviderProtocol, WorkSandboxPolicy,
};
pub use work_placement::{
    MAX_WORK_PLACEMENT_ROOT_BYTES, WorkPlacementBlockerV1, WorkPlacementContractError,
    WorkPlacementIdentityV1, WorkPlacementKindV1, WorkPlacementObservationV1,
    WorkPlacementPreflightV1, WorkPlacementStateV1, WorkPlacementTargetV1, WorkPlacementV1,
};
pub use work_product::{
    AcceptanceCriterionId, InitiativeId, MAX_WORK_PRODUCT_EVIDENCE, MAX_WORK_PRODUCT_ITEMS,
    MAX_WORK_PRODUCT_RELATIONS, MAX_WORK_PRODUCT_TEXT_BYTES, MilestoneId, TaskEvidenceLinkId,
    TaskEvidenceLinkV1, WorkAcceptanceCriterionV1, WorkGraphChangeV1, WorkGraphVersionV1,
    WorkHandoffId, WorkHandoffV1, WorkHierarchyV1, WorkInitiativeV1, WorkItemInputV1, WorkItemV1,
    WorkMilestoneV1, WorkPlanId, WorkPlanV1, WorkProductContractError, WorkProductGraphV1,
    WorkProductRelationV1, WorkProductSelectionScopeV1, WorkProposalDecisionV1,
    WorkProposalDispositionV1, WorkProposalV1, WorkProposedChildV1, WorkRelationReplanDecisionV1,
    WorkRelationReplanProposalV1, WorkRouteDecisionV1, WorkScoreKindV1, WorkShapeAssessmentV1,
    WorkSizingV1, WorkTaskEvidenceCoverageV1, WorkTaskEvidenceV1,
};
pub use work_product_event::{
    MAX_WORK_PRODUCT_EVENT_EVIDENCE, MAX_WORK_PRODUCT_EVENT_RELATION_SCOPES,
    MAX_WORK_PRODUCT_EVENT_SOURCE_WATERMARKS, WorkProductAuthorizedRelationScopeV1,
    WorkProductEventContractError, WorkProductEventEvidenceV1, WorkProductEventId,
    WorkProductEventInputV1, WorkProductEventPayloadV1, WorkProductEventSequenceV1,
    WorkProductEventV1, WorkProductProfileScopeV1, WorkProductSourceWatermarkV1,
};
pub use work_product_projection::{
    WorkCausalProjectionV1, WorkCriticalPathProjectionV1, WorkDagEdgeV1, WorkDagProjectionV1,
    WorkKanbanCardV1, WorkKanbanProjectionV1, WorkLegalActionV1, WorkProductProjectionBundleV1,
    WorkRuntimeAttemptProjectionV1, WorkRuntimeProjectionCoverageV1, WorkRuntimeProjectionV1,
    WorkTimelineEntryV1, WorkTimelineLaneV1, WorkTimelineProjectionV1, WorkWorkloadProjectionV1,
};
pub use work_read::{
    MAX_WORK_PROJECTION_CURSOR_BYTES, MAX_WORK_PROJECTION_READ_ITEMS, WorkProjectionCoverageV1,
    WorkProjectionDeltaV1, WorkProjectionReadError, WorkProjectionResumeCursorV1,
    WorkProjectionSequenceRangeV1, WorkProjectionSequenceV1, WorkProjectionSnapshotV1,
};
pub use work_routing::{
    WorkContentLocationClassV1, WorkEffortClassV1, WorkOrdinalBandV1, WorkRouteCandidateV1,
};
pub use work_run_control::{
    MAX_FENCED_WORK_ATTEMPTS, WorkBlockedIntervalCauseV1, WorkBlockedIntervalClosureV1,
    WorkBlockedIntervalIdentityV1, WorkBlockedIntervalReceiptV1, WorkRunControlAuthorityV1,
    WorkRunControlContractError, WorkRunControlReasonV1, WorkRunControlStateV1, WorkRunControlV1,
    WorkRunDeadlineCheckpointV1,
};
pub use work_runtime::{
    MAX_WORK_ATTEMPT_ARTIFACTS, MAX_WORK_INSTRUCTIONS_BYTES, WorkArtifactRefV1,
    WorkAttemptIdentityV1, WorkAttemptProgressV1, WorkAttemptProjectionBindingV1,
    WorkAttemptStateV1, WorkAttemptV1, WorkCancellationAcknowledgementV1,
    WorkCancellationEscalationV1, WorkCancellationRequestV1, WorkCancellationStateV1,
    WorkEffectStateV1, WorkExecutionBudgetV1, WorkExecutionEnvelopeV1, WorkFenceEpochV1,
    WorkLeaseFenceV1, WorkProviderBackendV1, WorkProviderRouteV1, WorkRecoveryStateV1,
    WorkRestartReasonV1, WorkRuntimeContractError, WorkTerminalEvidenceV1,
};
pub use workflow::{
    MAX_WORKFLOW_FAN_OUT, MAX_WORKFLOW_INPUTS, MAX_WORKFLOW_OUTPUTS, MAX_WORKFLOW_PREDECESSORS,
    MAX_WORKFLOW_STEPS, WorkflowDefinition, WorkflowDefinitionError, WorkflowFanOut,
    WorkflowOutputReference, WorkflowStep,
};
pub use workflow_fan_out_census::{
    WorkflowAttemptFrontierV1, WorkflowCensusCountV1, WorkflowCensusDurationV1,
    WorkflowCensusEvidenceReasonV1, WorkflowCensusGenerationV1,
    WorkflowExecutionTopologyClassificationV1, WorkflowExecutionTopologyEvidenceV1,
    WorkflowFanOutCensusV1, WorkflowProviderCapacityEvidenceV1, WorkflowProviderCapacityV1,
};
pub use workflow_receipt::{
    WorkflowPlacementReceipt, WorkflowReceiptError, WorkflowStepEffectOutcome,
    WorkflowStepEffectReceipt,
};
pub use workflow_run::{
    WorkflowFanOutChildPlanV1, WorkflowFanOutFailurePolicyV1, WorkflowFanOutPlanV1,
    WorkflowOutputArtifact, WorkflowRunCommand, WorkflowRunEvent, WorkflowRunEventContext,
    WorkflowRunEventKind, WorkflowRunProjection, WorkflowRunStateError, WorkflowRunStatus,
    WorkflowStepOutput, WorkflowStepRunProjection, WorkflowStepStatus,
};
