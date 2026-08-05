use super::*;

pub struct Pr13AdvisoryDaemonRegistrationV1<GR, GA, CS, CE, PE, PC> {
    pub advisory: Pr13AdvisoryRuntime<GR, GA, CS, CE, PE, PC>,
    pub feedback_owner: Arc<ConcretePr12FeedbackOwner>,
    pub publication_store: ProjectFeedbackStore,
    pub source_observations: Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
}

pub fn open_pr13_advisory_daemon_registration<GR, GA, CS, CE, PE, PC>(
    input: Pr13AdvisoryRuntimeOpenV1,
    providers: Pr13AdvisoryProviderAuthoritiesV1<GR, GA, CS, CE, PE, PC>,
) -> Result<Pr13AdvisoryDaemonRegistrationV1<GR, GA, CS, CE, PE, PC>, Pr13AdvisoryRuntimeOpenErrorV1>
where
    GR: GitHubCurrentBranchRemapper + Sync,
    GA: GitHubCanonicalReviewAnchorAuthorityV1 + Clone + Sync,
    CS: CiReadOnlyProviderArchiveV1 + Sync,
    CE: CiExactEvidenceAuthorityV1<CS::Record> + Sync,
    PE: CanonicalProximityEvidenceAuthorityV1 + Sync,
    PC: ConfigurationControlStore + Clone + Send + 'static,
{
    let advisory = Pr13AdvisoryRuntime::open(input, providers)?;
    let feedback_owner = advisory.feedback_owner();
    let publication_store = advisory.publication_store();
    let source_observations = advisory.source_observation_port();
    Ok(Pr13AdvisoryDaemonRegistrationV1 {
        advisory,
        feedback_owner,
        publication_store,
        source_observations,
    })
}
