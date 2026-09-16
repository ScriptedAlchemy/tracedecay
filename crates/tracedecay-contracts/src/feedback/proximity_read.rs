use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, ProximityAddressV1, ProximityCoverageV1, ProximityWarningClassV1,
};
use tracedecay_domain::{
    AgentInstanceId, CodeGenerationId, CommitId, FileOccurrenceId, ManifestDigest,
    ObservationSourceIdentityV1, RefId, RetrievalAnchorId, SourceSpan, SymbolOccurrenceId,
    UtcMicros, WorktreeId,
};

use crate::error::ApplicationContractError;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackProximityReadRequestV1 {
    pub observed_at: UtcMicros,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackProximityAccessKindV1 {
    Read,
    Write,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackProximityIntervalV1 {
    pub start: UtcMicros,
    pub end: UtcMicros,
}

impl FeedbackProximityIntervalV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.start.0 > self.end.0 {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback proximity interval",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackProximityParticipantV1 {
    pub source: ObservationSourceIdentityV1,
    pub agent_id: AgentInstanceId,
    pub worktree_id: Option<WorktreeId>,
    pub worktree_root: String,
    pub branch_ref: Option<RefId>,
    pub head_revision: Option<CommitId>,
    pub access: FeedbackProximityAccessKindV1,
    pub activity: FeedbackProximityIntervalV1,
    pub address: ProximityAddressV1,
}

impl FeedbackProximityParticipantV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.source
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity participant source",
            })?;
        self.agent_id
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity participant agent",
            })?;
        self.worktree_id
            .as_ref()
            .map_or(Ok(()), WorktreeId::validate)
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity participant worktree",
            })?;
        if self.worktree_root.is_empty()
            || self.worktree_root.trim() != self.worktree_root
            || self.worktree_root.chars().any(char::is_control)
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback proximity participant worktree root",
            });
        }
        self.branch_ref
            .as_ref()
            .map_or(Ok(()), RefId::validate)
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity participant branch",
            })?;
        self.head_revision
            .as_ref()
            .map_or(Ok(()), CommitId::validate)
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity participant revision",
            })?;
        self.activity.validate()?;
        self.address
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity participant address",
            })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackProximityCloneHandleV1 {
    pub source_generation: CodeGenerationId,
    pub source_symbol: SymbolOccurrenceId,
    pub retrieval_anchor_ids: Vec<RetrievalAnchorId>,
}

impl FeedbackProximityCloneHandleV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.source_generation
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity clone generation",
            })?;
        self.source_symbol
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity clone symbol",
            })?;
        if self.retrieval_anchor_ids.is_empty() {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback proximity clone anchors",
            });
        }
        for anchor in &self.retrieval_anchor_ids {
            anchor
                .validate()
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "feedback proximity clone anchor",
                })?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackProximityConflictDifferenceV1 {
    pub file: FileOccurrenceId,
    pub span: SourceSpan,
    pub difference_digest: ManifestDigest,
}

impl FeedbackProximityConflictDifferenceV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.file
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity conflict file",
            })?;
        self.span
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity conflict span",
            })?;
        self.difference_digest
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity conflict difference",
            })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackProximityConflictHandleV1 {
    pub common_base_revision: CommitId,
    pub left_head_revision: CommitId,
    pub right_head_revision: CommitId,
    pub evidence_digest: ManifestDigest,
    pub differences: Vec<FeedbackProximityConflictDifferenceV1>,
}

impl FeedbackProximityConflictHandleV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        for revision in [
            &self.common_base_revision,
            &self.left_head_revision,
            &self.right_head_revision,
        ] {
            revision
                .validate()
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "feedback proximity conflict revision",
                })?;
        }
        self.evidence_digest
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity conflict evidence",
            })?;
        if self.differences.is_empty() {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback proximity conflict differences",
            });
        }
        for difference in &self.differences {
            difference.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "relation_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FeedbackProximityRelationV1 {
    CodeNeighborhoodCandidate {
        warning_class: ProximityWarningClassV1,
    },
    SharedCodeCandidate {
        warning_class: ProximityWarningClassV1,
        clone_handle: FeedbackProximityCloneHandleV1,
    },
    OverlappingEdit {
        warning_class: ProximityWarningClassV1,
    },
    ConfirmedConflict {
        conflict_handle: FeedbackProximityConflictHandleV1,
    },
}

impl FeedbackProximityRelationV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        match self {
            Self::CodeNeighborhoodCandidate { warning_class } => {
                if matches!(
                    warning_class,
                    ProximityWarningClassV1::SameFile
                        | ProximityWarningClassV1::OverlappingRange
                        | ProximityWarningClassV1::SameSymbol
                ) {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "feedback proximity neighborhood relation",
                    });
                }
                Ok(())
            }
            Self::SharedCodeCandidate {
                warning_class,
                clone_handle,
            } => {
                if matches!(
                    warning_class,
                    ProximityWarningClassV1::SameFile
                        | ProximityWarningClassV1::OverlappingRange
                        | ProximityWarningClassV1::SameSymbol
                ) {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "feedback proximity shared-code relation",
                    });
                }
                clone_handle.validate()
            }
            Self::OverlappingEdit { warning_class } => {
                if !matches!(
                    warning_class,
                    ProximityWarningClassV1::SameFile
                        | ProximityWarningClassV1::OverlappingRange
                        | ProximityWarningClassV1::SameSymbol
                ) {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "feedback proximity overlap relation",
                    });
                }
                Ok(())
            }
            Self::ConfirmedConflict { conflict_handle } => conflict_handle.validate(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackProximityEncounterV1 {
    pub encounter_id: ManifestDigest,
    pub scope: FeedbackScopeV1,
    pub interval: FeedbackProximityIntervalV1,
    pub participants: Vec<FeedbackProximityParticipantV1>,
    pub relation: FeedbackProximityRelationV1,
    pub observed_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub coverage: ProximityCoverageV1,
}

impl FeedbackProximityEncounterV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.encounter_id
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity encounter identity",
            })?;
        self.scope
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity encounter scope",
            })?;
        self.interval.validate()?;
        if self.participants.len() != 2
            || self.observed_at.0 >= self.expires_at.0
            || !matches!(
                self.coverage,
                ProximityCoverageV1::Complete
                    | ProximityCoverageV1::Partial
                    | ProximityCoverageV1::Stale
            )
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback proximity encounter state",
            });
        }
        for participant in &self.participants {
            participant.validate()?;
            if participant.address.scope != self.scope {
                return Err(ApplicationContractError::Inconsistent {
                    field: "feedback proximity participant scope",
                });
            }
        }
        if self.participants[0].source == self.participants[1].source
            || self.participants[0].agent_id == self.participants[1].agent_id
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback proximity distinct participants",
            });
        }
        self.relation.validate()
    }
}

#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackProximityOmissionV1 {
    ActiveSessionLimit,
    SessionActivityLimit,
    RecentObservationLimit,
    EditedPathLimit,
    EncounterLimit,
    MissingParticipantObservation,
    MissingParticipantWorktree,
    MissingParticipantRevision,
    MissingCodeAddress,
    CloneCoveragePartial,
    ConflictEvidenceUnavailable,
    CodeIndexRevisionMismatch,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackProximityReadPageV1 {
    pub scope: FeedbackScopeV1,
    pub source_generation: CodeGenerationId,
    pub observed_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub encounters: Vec<FeedbackProximityEncounterV1>,
}

impl FeedbackProximityReadPageV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.scope
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity read scope",
            })?;
        self.source_generation
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "feedback proximity read generation",
            })?;
        if self.observed_at.0 >= self.expires_at.0 {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback proximity read expiry",
            });
        }
        for encounter in &self.encounters {
            encounter.validate()?;
            if encounter.scope != self.scope || encounter.expires_at.0 > self.expires_at.0 {
                return Err(ApplicationContractError::Inconsistent {
                    field: "feedback proximity read encounter binding",
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum FeedbackProximityReadResultV1 {
    Complete {
        page: FeedbackProximityReadPageV1,
    },
    CompleteZero {
        page: FeedbackProximityReadPageV1,
    },
    Partial {
        page: FeedbackProximityReadPageV1,
        omissions: Vec<FeedbackProximityOmissionV1>,
    },
    Stale {
        page: FeedbackProximityReadPageV1,
        omissions: Vec<FeedbackProximityOmissionV1>,
    },
    Denied {
        observed_at: UtcMicros,
    },
    Unavailable {
        observed_at: UtcMicros,
    },
}

impl FeedbackProximityReadResultV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        match self {
            Self::Complete { page } => {
                page.validate()?;
                if page.encounters.is_empty()
                    || page
                        .encounters
                        .iter()
                        .any(|encounter| encounter.coverage != ProximityCoverageV1::Complete)
                {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "feedback proximity complete result",
                    });
                }
            }
            Self::CompleteZero { page } => {
                page.validate()?;
                if !page.encounters.is_empty() {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "feedback proximity complete-zero result",
                    });
                }
            }
            Self::Partial { page, omissions } => {
                page.validate()?;
                if omissions.is_empty() {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "feedback proximity partial omissions",
                    });
                }
            }
            Self::Stale { page, omissions } => {
                page.validate()?;
                if omissions.is_empty()
                    || page
                        .encounters
                        .iter()
                        .any(|encounter| encounter.coverage != ProximityCoverageV1::Stale)
                {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "feedback proximity stale result",
                    });
                }
            }
            Self::Denied { .. } | Self::Unavailable { .. } => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{ProjectId, ProviderId, RepositoryId, SessionId, WorktreeId};

    use super::*;

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }

    fn scope() -> FeedbackScopeV1 {
        FeedbackScopeV1 {
            project_id: ProjectId::new("project.proximity-read").expect("project"),
            repository_id: RepositoryId::new("repository.proximity-read").expect("repository"),
            worktree_id: WorktreeId::new("worktree.proximity-read").expect("worktree"),
            branch_ref: "refs/heads/proximity-read".to_owned(),
            head_commit_id: CommitId::new("commit.proximity-read").expect("commit"),
        }
    }

    fn participant(provider: &str, session: &str, agent: &str) -> FeedbackProximityParticipantV1 {
        FeedbackProximityParticipantV1 {
            source: ObservationSourceIdentityV1::for_provider(
                ProviderId::new(provider).expect("provider"),
                SessionId::new(session).expect("session"),
            )
            .expect("source"),
            agent_id: AgentInstanceId::new(agent).expect("agent"),
            worktree_id: Some(scope().worktree_id),
            worktree_root: format!("/tmp/{session}"),
            branch_ref: Some(RefId::new("refs/heads/proximity-read").expect("ref")),
            head_revision: Some(CommitId::new(format!("commit.{session}")).expect("head")),
            access: FeedbackProximityAccessKindV1::Write,
            activity: FeedbackProximityIntervalV1 {
                start: UtcMicros(10),
                end: UtcMicros(20),
            },
            address: ProximityAddressV1 {
                scope: scope(),
                file: FileOccurrenceId::new("file.proximity").expect("file"),
                span: Some(SourceSpan {
                    start_byte: 4,
                    end_byte: 12,
                }),
                symbol: Some(SymbolOccurrenceId::new("symbol.proximity").expect("symbol")),
            },
        }
    }

    fn encounter(relation: FeedbackProximityRelationV1) -> FeedbackProximityEncounterV1 {
        FeedbackProximityEncounterV1 {
            encounter_id: digest('a'),
            scope: scope(),
            interval: FeedbackProximityIntervalV1 {
                start: UtcMicros(10),
                end: UtcMicros(20),
            },
            participants: vec![
                participant("codex", "session.left", "agent.left"),
                participant("cursor", "session.right", "agent.right"),
            ],
            relation,
            observed_at: UtcMicros(20),
            expires_at: UtcMicros(30),
            coverage: ProximityCoverageV1::Complete,
        }
    }

    fn page(encounters: Vec<FeedbackProximityEncounterV1>) -> FeedbackProximityReadPageV1 {
        FeedbackProximityReadPageV1 {
            scope: scope(),
            source_generation: CodeGenerationId::new("generation.proximity").expect("generation"),
            observed_at: UtcMicros(20),
            expires_at: UtcMicros(30),
            encounters,
        }
    }

    #[test]
    fn same_range_conflict_requires_exact_handle() {
        let conflict = FeedbackProximityRelationV1::ConfirmedConflict {
            conflict_handle: FeedbackProximityConflictHandleV1 {
                common_base_revision: CommitId::new("commit.base").expect("base"),
                left_head_revision: CommitId::new("commit.left").expect("left"),
                right_head_revision: CommitId::new("commit.right").expect("right"),
                evidence_digest: digest('b'),
                differences: vec![FeedbackProximityConflictDifferenceV1 {
                    file: FileOccurrenceId::new("file.proximity").expect("file"),
                    span: SourceSpan {
                        start_byte: 4,
                        end_byte: 12,
                    },
                    difference_digest: digest('c'),
                }],
            },
        };
        encounter(conflict).validate().expect("confirmed conflict");
    }

    #[test]
    fn same_file_overlap_stays_distinct_from_conflict() {
        let overlap = encounter(FeedbackProximityRelationV1::OverlappingEdit {
            warning_class: ProximityWarningClassV1::SameFile,
        });
        overlap.validate().expect("same-file overlap");
        assert!(matches!(
            overlap.relation,
            FeedbackProximityRelationV1::OverlappingEdit { .. }
        ));
    }

    #[test]
    fn shared_code_candidate_requires_clone_handle() {
        let shared = encounter(FeedbackProximityRelationV1::SharedCodeCandidate {
            warning_class: ProximityWarningClassV1::SharedDependency,
            clone_handle: FeedbackProximityCloneHandleV1 {
                source_generation: CodeGenerationId::new("generation.proximity")
                    .expect("generation"),
                source_symbol: SymbolOccurrenceId::new("symbol.proximity").expect("symbol"),
                retrieval_anchor_ids: vec![
                    RetrievalAnchorId::new("anchor.proximity").expect("anchor"),
                ],
            },
        });
        shared.validate().expect("shared-code candidate");
    }

    #[test]
    fn denied_disclosure_contains_no_participant_or_observation_data() {
        let denied = FeedbackProximityReadResultV1::Denied {
            observed_at: UtcMicros(20),
        };
        denied.validate().expect("denied result");
        let encoded = serde_json::to_value(denied).expect("serialize denied result");
        assert!(encoded.get("participants").is_none());
        assert!(encoded.get("observations").is_none());
    }

    #[test]
    fn partial_clone_coverage_preserves_omission() {
        let mut shared = encounter(FeedbackProximityRelationV1::SharedCodeCandidate {
            warning_class: ProximityWarningClassV1::SharedCaller,
            clone_handle: FeedbackProximityCloneHandleV1 {
                source_generation: CodeGenerationId::new("generation.proximity")
                    .expect("generation"),
                source_symbol: SymbolOccurrenceId::new("symbol.proximity").expect("symbol"),
                retrieval_anchor_ids: vec![
                    RetrievalAnchorId::new("anchor.proximity").expect("anchor"),
                ],
            },
        });
        shared.coverage = ProximityCoverageV1::Partial;
        let result = FeedbackProximityReadResultV1::Partial {
            page: page(vec![shared]),
            omissions: vec![FeedbackProximityOmissionV1::CloneCoveragePartial],
        };
        result.validate().expect("partial clone coverage");
    }

    #[test]
    fn complete_zero_encounters_is_an_explicit_state() {
        let result = FeedbackProximityReadResultV1::CompleteZero {
            page: page(Vec::new()),
        };
        result.validate().expect("complete zero");
    }
}
