use super::*;

pub struct AdvisoryDaemonRegistrationV1<GR, GA, CS, CE, PE, PC> {
    pub advisory: AdvisoryRuntime<GR, GA, CS, CE, PE, PC>,
    pub feedback_owner: Arc<ConcreteFeedbackOwner>,
    pub publication_store: ProjectFeedbackStore,
    pub source_observations: Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
}

pub fn open_advisory_daemon_registration<GR, GA, CS, CE, PE, PC>(
    input: AdvisoryRuntimeOpenV1,
    providers: AdvisoryProviderAuthoritiesV1<GR, GA, CS, CE, PE, PC>,
) -> Result<AdvisoryDaemonRegistrationV1<GR, GA, CS, CE, PE, PC>, AdvisoryRuntimeOpenErrorV1>
where
    GR: GitHubCurrentBranchRemapper + Sync,
    GA: GitHubCanonicalReviewAnchorAuthorityV1 + Clone + Sync,
    CS: CiReadOnlyProviderArchiveV1 + Sync,
    CE: CiExactEvidenceAuthorityV1<CS::Record> + Sync,
    PE: CanonicalProximityEvidenceAuthorityV1 + Sync,
    PC: ConfigurationControlStore + Clone + Send + 'static,
{
    let advisory = AdvisoryRuntime::open(input, providers)?;
    let feedback_owner = advisory.feedback_owner();
    let publication_store = advisory.publication_store();
    let source_observations = advisory.source_observation_port();
    Ok(AdvisoryDaemonRegistrationV1 {
        advisory,
        feedback_owner,
        publication_store,
        source_observations,
    })
}
