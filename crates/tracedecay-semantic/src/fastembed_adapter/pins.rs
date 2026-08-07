/// Exact signed-manifest pin that failed projection/artifact admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionArtifactPinV1 {
    ArtifactIdentity,
    ManifestIdentity,
    ProfileKind,
    ArtifactDigest,
    TokenizerDigest,
    ConfigDigest,
    QueryInstructionDigest,
    DocumentInstructionDigest,
    Pooling,
    TruncationSide,
    TruncationLength,
    RuntimeBackend,
    RuntimeBuildRevision,
    DeviceClass,
    Dimensions,
    Metric,
    Normalization,
    Precision,
}
