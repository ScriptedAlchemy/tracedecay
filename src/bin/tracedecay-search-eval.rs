use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tracedecay::search_eval::holdout::{
    AgentDelegationPayloadV1, AgentJudgmentArtifactV1, HoldoutAuthorityStoreV1,
};
use tracedecay::search_eval::{
    CompareOptions, SealHoldoutOptions, compare, seal_holdout_labels, validate_fixture_root,
};
use tracedecay_domain::{
    DecisionOwnerId, EvalOutcomeV1, EvidenceIndexV1, FixtureContentDigest, HoldoutLabelAuthorityV1,
    HoldoutRevealCapabilityV1, HoldoutSealV1, RunManifestV1,
};

#[derive(Debug, Parser)]
#[command(
    name = "tracedecay-search-eval",
    about = "Validate and compare frozen Plan 15 search-quality runs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate all checked-in fixture bytes and canonical digests.
    Validate {
        #[arg(long, default_value = "tests/fixtures/search_quality")]
        fixtures: PathBuf,
    },
    /// Compute the canonical digest for a run manifest draft.
    RehashRun {
        #[arg(long)]
        input: PathBuf,
    },
    /// Compute the canonical digest for an evidence-index draft.
    RehashEvidence {
        #[arg(long)]
        input: PathBuf,
    },
    /// Compare one frozen run and emit one immutable terminal outcome.
    Compare {
        #[arg(long, default_value = "tests/fixtures/search_quality")]
        fixtures: PathBuf,
        #[arg(long)]
        run_manifest: Option<PathBuf>,
        #[arg(long, default_value = "benchmarks/search-quality/runs")]
        output_root: PathBuf,
        #[arg(long, value_name = "AUTHORIZED_STORE_LOCATOR")]
        holdout_capability: Option<String>,
        #[arg(long)]
        saved_candidates: Option<PathBuf>,
        #[arg(long)]
        require_outcome: Option<OutcomeArg>,
    },
    /// Import one blinded packet, signed delegation, or immutable judgment.
    Packet {
        #[arg(long)]
        profile_root: Option<PathBuf>,
        #[arg(long, value_enum)]
        kind: PacketKindArg,
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        trust_root: Option<String>,
        #[arg(long)]
        blinded_packet_digest: Option<String>,
        #[arg(long)]
        at_unix: u64,
    },
    /// Validate, import, and sign one private sealed-label packet.
    Seal {
        #[arg(long, default_value = "tests/fixtures/search_quality")]
        fixtures: PathBuf,
        #[arg(long)]
        run_manifest: Option<PathBuf>,
        #[arg(long)]
        labels: PathBuf,
        #[arg(long)]
        profile_root: Option<PathBuf>,
        #[arg(long, value_enum)]
        label_authority: LabelAuthorityArg,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        trust_root: Option<String>,
        #[arg(long)]
        at_unix: u64,
    },
    /// Mint one least-privilege run-bound reveal capability.
    Capability {
        #[arg(long)]
        run_manifest: PathBuf,
        #[arg(long)]
        seal: PathBuf,
        #[arg(long)]
        profile_root: Option<PathBuf>,
        #[arg(long)]
        trust_root: Option<String>,
        #[arg(long)]
        not_before_unix: u64,
        #[arg(long)]
        expires_at_unix: u64,
        #[arg(long)]
        at_unix: u64,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "snake_case")]
enum PacketKindArg {
    Blinded,
    Delegation,
    Judgment,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "snake_case")]
enum LabelAuthorityArg {
    Deterministic,
    HumanAuthoritative,
    AgentAdjudicated,
}

impl From<LabelAuthorityArg> for HoldoutLabelAuthorityV1 {
    fn from(value: LabelAuthorityArg) -> Self {
        match value {
            LabelAuthorityArg::Deterministic => Self::Deterministic,
            LabelAuthorityArg::HumanAuthoritative => Self::HumanAuthoritative,
            LabelAuthorityArg::AgentAdjudicated => Self::AgentAdjudicated,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "snake_case")]
enum OutcomeArg {
    InvalidRun,
    Blocked,
    Rejected,
    Inconclusive,
    RuntimeFallbackObserved,
    Accepted,
}

impl From<OutcomeArg> for EvalOutcomeV1 {
    fn from(value: OutcomeArg) -> Self {
        match value {
            OutcomeArg::InvalidRun => Self::InvalidRun,
            OutcomeArg::Blocked => Self::Blocked,
            OutcomeArg::Rejected => Self::Rejected,
            OutcomeArg::Inconclusive => Self::Inconclusive,
            OutcomeArg::RuntimeFallbackObserved => Self::RuntimeFallbackObserved,
            OutcomeArg::Accepted => Self::Accepted,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate { fixtures } => match validate_fixture_root(&fixtures) {
            Ok(summary) => emit(&summary, ExitCode::SUCCESS),
            Err(error) => emit(
                &json!({
                    "command": "validate",
                    "status": "invalid",
                    "outcome": EvalOutcomeV1::InvalidRun,
                    "rationale": error.to_string(),
                }),
                ExitCode::from(2),
            ),
        },
        Command::RehashRun { input } => emit_operation(
            "rehash_run",
            (|| {
                let run: RunManifestV1 = read_json_file(&input)?;
                let digest = run.compute_digest().map_err(|error| error.to_string())?;
                serde_json::to_value(digest).map_err(|error| error.to_string())
            })(),
        ),
        Command::RehashEvidence { input } => emit_operation(
            "rehash_evidence",
            (|| {
                let index: EvidenceIndexV1 = read_json_file(&input)?;
                let digest = index.compute_digest().map_err(|error| error.to_string())?;
                serde_json::to_value(digest).map_err(|error| error.to_string())
            })(),
        ),
        Command::Compare {
            fixtures,
            run_manifest,
            output_root,
            holdout_capability,
            saved_candidates,
            require_outcome,
        } => {
            let required_outcome = require_outcome.map(Into::into);
            let options = CompareOptions {
                fixture_root: fixtures,
                run_manifest,
                output_root,
                holdout_capability,
                saved_candidates,
                required_outcome,
            };
            match compare(&options) {
                Ok(result) => {
                    let exit = if result.requirement_satisfied {
                        ExitCode::SUCCESS
                    } else {
                        outcome_exit(result.outcome)
                    };
                    emit(&result, exit)
                }
                Err(error) => emit(
                    &json!({
                        "command": "compare",
                        "outcome": EvalOutcomeV1::InvalidRun,
                        "required_outcome": required_outcome,
                        "requirement_satisfied": required_outcome == Some(EvalOutcomeV1::InvalidRun),
                        "rationale": error.to_string(),
                    }),
                    ExitCode::from(2),
                ),
            }
        }
        Command::Packet {
            profile_root,
            kind,
            input,
            owner,
            trust_root,
            blinded_packet_digest,
            at_unix,
        } => emit_operation(
            "packet",
            (|| {
                let store = authority_store(profile_root.as_deref())?;
                let record = match kind {
                    PacketKindArg::Blinded => {
                        let path = input.ok_or("--input is required for blinded packets")?;
                        let bytes = std::fs::read(&path)
                            .map_err(|error| format!("read {}: {error}", path.display()))?;
                        store
                            .import_blinded_packet(&bytes, at_unix)
                            .map_err(|error| error.to_string())?
                    }
                    PacketKindArg::Delegation => {
                        let delegated_by = DecisionOwnerId::new(
                            owner.ok_or("--owner is required for delegations")?,
                        )
                        .map_err(|error| error.to_string())?;
                        let packet_digest = FixtureContentDigest::new(
                            blinded_packet_digest
                                .ok_or("--blinded-packet-digest is required for delegations")?,
                        )
                        .map_err(|error| error.to_string())?;
                        store
                            .sign_agent_delegation(
                                AgentDelegationPayloadV1 {
                                    schema_revision: 1,
                                    delegated_by,
                                    blinded_packet_digest: packet_digest,
                                    signed_at_unix: at_unix,
                                },
                                trust_root.as_deref().unwrap_or(""),
                            )
                            .map_err(|error| error.to_string())?
                    }
                    PacketKindArg::Judgment => {
                        let path = input.ok_or("--input is required for judgment packets")?;
                        let artifact: AgentJudgmentArtifactV1 = read_json_file(&path)?;
                        store
                            .import_agent_judgment(&artifact)
                            .map_err(|error| error.to_string())?
                    }
                };
                serde_json::to_value(record).map_err(|error| error.to_string())
            })(),
        ),
        Command::Seal {
            fixtures,
            run_manifest,
            labels,
            profile_root,
            label_authority,
            owner,
            trust_root,
            at_unix,
        } => emit_operation(
            "seal",
            (|| {
                let signed_by = DecisionOwnerId::new(owner).map_err(|error| error.to_string())?;
                let result = seal_holdout_labels(&SealHoldoutOptions {
                    fixture_root: fixtures,
                    run_manifest,
                    labels_path: labels,
                    profile_root,
                    label_authority: label_authority.into(),
                    signed_by,
                    trust_root_id: trust_root.unwrap_or_default(),
                    signed_at_unix: at_unix,
                })
                .map_err(|error| error.to_string())?;
                serde_json::to_value(result).map_err(|error| error.to_string())
            })(),
        ),
        Command::Capability {
            run_manifest,
            seal,
            profile_root,
            trust_root,
            not_before_unix,
            expires_at_unix,
            at_unix,
        } => emit_operation(
            "capability",
            (|| {
                let run: RunManifestV1 = read_json_file(&run_manifest)?;
                let seal: HoldoutSealV1 = read_json_file(&seal)?;
                let revealed_by = run
                    .decision_owners
                    .first()
                    .cloned()
                    .ok_or("run manifest has no decision owner")?;
                let store = authority_store(profile_root.as_deref())?;
                let record = store
                    .sign_reveal_capability(
                        HoldoutRevealCapabilityV1 {
                            schema_revision: 1,
                            labels_locator: seal.locator,
                            envelope_locator: seal.signature_locator,
                            seal_digest: seal.seal_digest,
                            run_id: run.run_id,
                            run_manifest_digest: run.digest,
                            revealed_by,
                            operation: "evaluate_locked_quality_v1".to_string(),
                            not_before_unix,
                            expires_at_unix,
                        },
                        trust_root.as_deref().unwrap_or(""),
                        at_unix,
                    )
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(record).map_err(|error| error.to_string())
            })(),
        ),
    }
}

fn authority_store(
    profile_root: Option<&std::path::Path>,
) -> Result<HoldoutAuthorityStoreV1, String> {
    match profile_root {
        Some(root) => HoldoutAuthorityStoreV1::open_at(root),
        None => HoldoutAuthorityStoreV1::open_default(),
    }
    .map_err(|error| error.to_string())
}

fn read_json_file<T: DeserializeOwned>(path: &std::path::Path) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn emit_operation(command: &'static str, result: Result<serde_json::Value, String>) -> ExitCode {
    match result {
        Ok(value) => emit(
            &json!({"command": command, "status": "ok", "result": value}),
            ExitCode::SUCCESS,
        ),
        Err(error) => emit(
            &json!({"command": command, "status": "invalid", "rationale": error}),
            ExitCode::from(2),
        ),
    }
}

fn outcome_exit(outcome: EvalOutcomeV1) -> ExitCode {
    match outcome {
        EvalOutcomeV1::Accepted => ExitCode::SUCCESS,
        EvalOutcomeV1::InvalidRun => ExitCode::from(2),
        EvalOutcomeV1::Blocked => ExitCode::from(3),
        EvalOutcomeV1::Rejected => ExitCode::from(4),
        EvalOutcomeV1::Inconclusive => ExitCode::from(5),
        EvalOutcomeV1::RuntimeFallbackObserved => ExitCode::from(6),
    }
}

fn emit<T: Serialize>(value: &T, success: ExitCode) -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer_pretty(&mut stdout, value)
        .and_then(|()| {
            use std::io::Write as _;
            stdout.write_all(b"\n").map_err(serde_json::Error::io)
        })
        .is_err()
    {
        eprintln!("failed to write evaluator result");
        ExitCode::from(2)
    } else {
        success
    }
}
