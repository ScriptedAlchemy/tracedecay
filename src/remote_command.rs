//! CLI presentation for the Remote Brain operator plane.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tracedecay_application::RemoteListenerReadV1;
use tracedecay_application::remote::protocol::{
    EnrollmentRequestV1, RemoteProtocolRequestV1, RemoteProtocolResponseV1,
};
use tracedecay_application::remote::status::{
    RemoteOperationalReadinessV1, RemoteOperationalStatusReadV1, RemoteOperationalStatusV1,
};
use tracedecay_domain::CurrentRemoteAuthorityStateV1;
use tracedecay_sdk::remote_client::{EnrolledRemoteClient, RemoteClientError};

use crate::errors::{Result, TraceDecayError};

pub const DEFAULT_REMOTE_TIMEOUT_SECS: u64 = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteProtocolArgs {
    pub endpoint: String,
    pub credential_file: PathBuf,
    pub trust_root_file: Option<PathBuf>,
    pub timeout_secs: u64,
    pub request_file: PathBuf,
    pub json: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteCommand {
    Status {
        json: bool,
    },
    Enroll {
        args: RemoteProtocolArgs,
        enrollment_credential_file: PathBuf,
    },
    Replay {
        args: RemoteProtocolArgs,
    },
    Backup {
        args: RemoteProtocolArgs,
    },
    Restore {
        args: RemoteProtocolArgs,
    },
    Failover {
        args: RemoteProtocolArgs,
    },
}

pub fn run(command: RemoteCommand) -> Result<()> {
    match command {
        RemoteCommand::Status { json } => run_status(json),
        RemoteCommand::Enroll {
            args,
            enrollment_credential_file,
        } => run_enroll(args, enrollment_credential_file),
        RemoteCommand::Replay { args } => {
            let request = read_protocol_request(&args.request_file)?;
            let client = build_client(&args)?;
            emit_protocol_response(
                &client.replay(&request).map_err(map_remote_client_error)?,
                args.json,
            )
        }
        RemoteCommand::Backup { args } => {
            let request = read_protocol_request(&args.request_file)?;
            let client = build_client(&args)?;
            emit_protocol_response(
                &client.backup(&request).map_err(map_remote_client_error)?,
                args.json,
            )
        }
        RemoteCommand::Restore { args } => {
            let request = read_protocol_request(&args.request_file)?;
            let client = build_client(&args)?;
            emit_protocol_response(
                &client.restore(&request).map_err(map_remote_client_error)?,
                args.json,
            )
        }
        RemoteCommand::Failover { args } => {
            let request = read_protocol_request(&args.request_file)?;
            let client = build_client(&args)?;
            emit_protocol_response(
                &client.failover(&request).map_err(map_remote_client_error)?,
                args.json,
            )
        }
    }
}

fn run_status(json: bool) -> Result<()> {
    let status = crate::daemon::live_remote_operational_status()?;
    if json {
        print!("{}", status_json_line(&status)?);
    } else {
        print!("{}", render_status_human(&status));
    }
    Ok(())
}

fn run_enroll(args: RemoteProtocolArgs, enrollment_credential_file: PathBuf) -> Result<()> {
    let request: RemoteProtocolRequestV1<EnrollmentRequestV1> =
        read_protocol_request(&args.request_file)?;
    let enrollment_credential =
        std::fs::read(&enrollment_credential_file).map_err(|error| TraceDecayError::File {
            message: format!("failed to read Remote Brain enrollment credential file: {error}"),
            path: enrollment_credential_file.display().to_string(),
        })?;
    let client = build_client(&args)?;
    emit_protocol_response(
        &client
            .enroll(&request, enrollment_credential)
            .map_err(map_remote_client_error)?,
        args.json,
    )
}

fn build_client(args: &RemoteProtocolArgs) -> Result<EnrolledRemoteClient> {
    if args.timeout_secs == 0 {
        return Err(TraceDecayError::Config {
            message: "Remote Brain --timeout-secs must be greater than zero".to_owned(),
        });
    }
    let credential =
        std::fs::read(&args.credential_file).map_err(|error| TraceDecayError::File {
            message: format!("failed to read Remote Brain credential file: {error}"),
            path: args.credential_file.display().to_string(),
        })?;
    let timeout = Duration::from_secs(args.timeout_secs);
    match &args.trust_root_file {
        Some(path) => {
            let pem = std::fs::read(path).map_err(|error| TraceDecayError::File {
                message: format!("failed to read Remote Brain trust-root file: {error}"),
                path: path.display().to_string(),
            })?;
            EnrolledRemoteClient::new_with_root_certificate(
                &args.endpoint,
                credential,
                timeout,
                pem,
            )
        }
        None => EnrolledRemoteClient::new(&args.endpoint, credential, timeout),
    }
    .map_err(map_remote_client_error)
}

fn read_protocol_request<T: DeserializeOwned>(path: &Path) -> Result<RemoteProtocolRequestV1<T>> {
    let payload = if path == Path::new("-") {
        let mut payload = String::new();
        std::io::stdin().read_to_string(&mut payload)?;
        payload
    } else {
        std::fs::read_to_string(path).map_err(|error| TraceDecayError::File {
            message: format!("failed to read Remote Brain request file: {error}"),
            path: path.display().to_string(),
        })?
    };
    serde_json::from_str(&payload).map_err(|error| TraceDecayError::Config {
        message: format!(
            "Remote Brain request file {} is not valid JSON for this operation: {error}",
            path.display()
        ),
    })
}

fn emit_protocol_response<T: Serialize>(
    response: &RemoteProtocolResponseV1<T>,
    json: bool,
) -> Result<()> {
    if json {
        print!("{}", canonical_json_line(response)?);
    } else {
        print!("{}", render_protocol_response(response));
    }
    match &response.result {
        Ok(_) => Ok(()),
        Err(problem) => Err(TraceDecayError::Config {
            message: format!(
                "Remote Brain request {} failed: {}: {}",
                response.request_id, problem.problem.code, problem.problem.message
            ),
        }),
    }
}

fn map_remote_client_error(error: RemoteClientError) -> TraceDecayError {
    TraceDecayError::Config {
        message: error.to_string(),
    }
}

fn canonical_json_line<T: Serialize>(value: &T) -> serde_json::Result<String> {
    let mut rendered = serde_json::to_string(value)?;
    rendered.push('\n');
    Ok(rendered)
}

fn status_json_line(status: &RemoteOperationalStatusReadV1) -> serde_json::Result<String> {
    canonical_json_line(status)
}

fn render_status_human(status: &RemoteOperationalStatusReadV1) -> String {
    match status {
        RemoteOperationalStatusReadV1::Observed {
            listener, status, ..
        } => render_observed_status(*listener, status),
        RemoteOperationalStatusReadV1::Unconfigured => "Remote Brain: unconfigured\n".to_owned(),
        RemoteOperationalStatusReadV1::Unavailable => "Remote Brain: unavailable\n".to_owned(),
    }
}

fn render_observed_status(
    listener: RemoteListenerReadV1,
    status: &RemoteOperationalStatusV1,
) -> String {
    format!(
        "Remote Brain: {}\n\
Listener: {}\n\
Authority: {}\n\
Enrollment configured: {}\n\
Spool pending: {}\n\
Spool quarantined: {}\n\
Sequence gap: {}\n\
Replay coverage complete: {}\n\
Current backup verified: {}\n\
Failover in progress: {}\n\
Recovery required: {}\n",
        readiness_label(status.readiness),
        listener_label(listener),
        authority_label(&status.authority),
        yes_no(status.enrollment_configured),
        status.spool.pending_count,
        status.spool.quarantined_count,
        yes_no(status.spool.has_sequence_gap),
        yes_no(status.replay_coverage_complete),
        yes_no(status.current_backup_verified),
        yes_no(status.failover_in_progress),
        yes_no(status.recovery_required),
    )
}

fn render_protocol_response<T>(response: &RemoteProtocolResponseV1<T>) -> String {
    let outcome = match &response.result {
        Ok(_) => "ok".to_owned(),
        Err(problem) => format!("{}: {}", problem.problem.code, problem.problem.message),
    };
    format!(
        "Request: {}\nAuthority: {}\nOutcome: {}\n",
        response.request_id,
        authority_label(&response.authority),
        outcome,
    )
}

fn readiness_label(readiness: RemoteOperationalReadinessV1) -> &'static str {
    match readiness {
        RemoteOperationalReadinessV1::Unconfigured => "unconfigured",
        RemoteOperationalReadinessV1::Partial => "partial",
        RemoteOperationalReadinessV1::Ready => "ready",
        RemoteOperationalReadinessV1::RecoveryRequired => "recovery_required",
    }
}

fn listener_label(listener: RemoteListenerReadV1) -> &'static str {
    match listener {
        RemoteListenerReadV1::Serving => "serving",
        RemoteListenerReadV1::Disabled => "disabled",
        RemoteListenerReadV1::Degraded => "degraded",
    }
}

fn authority_label(authority: &CurrentRemoteAuthorityStateV1) -> &'static str {
    match authority {
        CurrentRemoteAuthorityStateV1::Available(_) => "available",
        CurrentRemoteAuthorityStateV1::Partial { .. } => "partial",
        CurrentRemoteAuthorityStateV1::Unavailable { .. } => "unavailable",
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        RemoteProtocolArgs, build_client, canonical_json_line, emit_protocol_response,
        render_protocol_response, render_status_human, status_json_line,
    };
    use tracedecay_application::remote::protocol::{
        RemoteProtocolFailureV1, RemoteProtocolResponseV1, remote_enrollment_result_contract_v1,
        remote_protocol_problem,
    };
    use tracedecay_application::remote::status::{
        RemoteOperationalStatusReadV1, RemoteOperationalStatusV1,
    };
    use tracedecay_application::{DoctorCoverageCompletenessV1, RemoteListenerReadV1, RequestId};
    use tracedecay_domain::{
        CurrentRemoteAuthorityStateV1, RemoteAuthorityUnavailableReasonV1, UtcMicros,
    };

    use crate::errors::TraceDecayError;

    fn observed_fixture() -> RemoteOperationalStatusReadV1 {
        let status: RemoteOperationalStatusV1 = serde_json::from_value(serde_json::json!({
            "readiness": "partial",
            "enrollment_configured": true,
            "authority": {
                "state": "available",
                "value": {
                    "fence": {
                        "brain_id": "brain.status",
                        "shard_id": "shard.status",
                        "generation_id": "generation.status",
                        "placement_revision": 1,
                        "authority_epoch": 1,
                        "authority_node_id": "node.authority"
                    },
                    "credential_revision": 1,
                    "observed_at": 10
                }
            },
            "spool": {
                "pending_count": 3,
                "quarantined_count": 1,
                "has_sequence_gap": true
            },
            "replay_coverage_complete": false,
            "current_backup_verified": true,
            "failover_in_progress": false,
            "recovery_required": false,
            "observed_at": 10
        }))
        .expect("observed Remote operational status fixture");
        RemoteOperationalStatusReadV1::Observed {
            listener: RemoteListenerReadV1::Serving,
            status,
            coverage: DoctorCoverageCompletenessV1::Partial,
        }
    }

    #[test]
    fn status_json_line_emits_one_line_for_each_read_variant() {
        for status in [
            observed_fixture(),
            RemoteOperationalStatusReadV1::Unconfigured,
            RemoteOperationalStatusReadV1::Unavailable,
        ] {
            let rendered = status_json_line(&status).expect("status JSON line");
            assert!(rendered.ends_with('\n'));
            assert_eq!(rendered.lines().count(), 1);
            let decoded: RemoteOperationalStatusReadV1 =
                serde_json::from_str(rendered.trim_end()).expect("round-trip status JSON");
            assert_eq!(decoded, status);
        }
    }

    #[test]
    fn status_human_render_covers_all_read_variants() {
        let observed = render_status_human(&observed_fixture());
        assert!(observed.contains("Remote Brain: partial"));
        assert!(observed.contains("Listener: serving"));
        assert!(observed.contains("Authority: available"));
        assert!(observed.contains("Enrollment configured: yes"));
        assert!(observed.contains("Spool pending: 3"));
        assert!(observed.contains("Spool quarantined: 1"));
        assert!(observed.contains("Sequence gap: yes"));
        assert!(observed.contains("Replay coverage complete: no"));
        assert!(observed.contains("Current backup verified: yes"));
        assert!(observed.contains("Failover in progress: no"));
        assert!(observed.contains("Recovery required: no"));

        assert_eq!(
            render_status_human(&RemoteOperationalStatusReadV1::Unconfigured),
            "Remote Brain: unconfigured\n"
        );
        assert_eq!(
            render_status_human(&RemoteOperationalStatusReadV1::Unavailable),
            "Remote Brain: unavailable\n"
        );
    }

    fn protocol_problem_response() -> RemoteProtocolResponseV1<()> {
        let request_id =
            RequestId::new("request.cli.remote.7").expect("canonical remote request id");
        RemoteProtocolResponseV1::new(
            request_id.clone(),
            CurrentRemoteAuthorityStateV1::Unavailable {
                reason: RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable,
                observed_at: UtcMicros(20),
            },
            Err(remote_protocol_problem(
                remote_enrollment_result_contract_v1(),
                request_id,
                RemoteProtocolFailureV1::AuthorityUnavailable,
            )
            .expect("typed remote protocol problem")),
        )
        .expect("typed remote protocol response")
    }

    #[test]
    fn protocol_json_and_human_render_cover_typed_problem() {
        let response = protocol_problem_response();
        let rendered = canonical_json_line(&response).expect("protocol JSON line");
        assert!(rendered.ends_with('\n'));
        assert_eq!(rendered.lines().count(), 1);
        assert!(rendered.contains("request.cli.remote.7"));

        let Err(problem) = &response.result else {
            panic!("expected typed protocol problem");
        };
        let outcome = format!("{}: {}", problem.problem.code, problem.problem.message);
        assert_eq!(
            render_protocol_response(&response),
            format!("Request: request.cli.remote.7\nAuthority: unavailable\nOutcome: {outcome}\n")
        );
    }

    #[test]
    fn emit_protocol_response_returns_config_error_for_typed_problem() {
        let response = protocol_problem_response();
        let error = emit_protocol_response(&response, true)
            .expect_err("typed Remote Brain problem must be non-zero");
        match error {
            TraceDecayError::Config { message } => {
                assert!(message.contains("request.cli.remote.7"));
                assert!(message.contains(&response.result.as_ref().unwrap_err().problem.code));
            }
            other => panic!("expected config error, got {other:?}"),
        }
    }

    #[test]
    fn build_client_rejects_zero_timeout_before_reading_files() {
        let error = build_client(&RemoteProtocolArgs {
            endpoint: "https://brain.example/remote/".to_owned(),
            credential_file: PathBuf::from("/this/file/must-not-be-read.bin"),
            trust_root_file: None,
            timeout_secs: 0,
            request_file: PathBuf::from("request.json"),
            json: false,
        })
        .expect_err("zero timeout must fail closed");
        match error {
            TraceDecayError::Config { message } => {
                assert!(message.contains("--timeout-secs"));
            }
            other => panic!("expected config error, got {other:?}"),
        }
    }
}
