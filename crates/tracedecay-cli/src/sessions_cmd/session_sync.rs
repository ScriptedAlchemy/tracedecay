use std::path::Path;
use tracedecay_contracts::retrieval::{AdminCliSessionSyncV1, AdminCliSurfaceRequestV1};
use tracedecay_contracts::session_sync::{
    SessionSyncCoverageV1, SessionSyncSourceCoverageV1, SessionSyncSourceV1,
};
use tracedecay_contracts::{IdempotencyKey, OperationTermination, RequestId};
use tracedecay_runtime_core::config::ProfileRoot;

use super::{resolve_cli_project_root, session_sync_action};

/// Default lower bound for `git-sync`: 90 days before now.
const GIT_SYNC_DEFAULT_WINDOW_SECS: i64 = 90 * 24 * 60 * 60;

pub(super) async fn run_git_sync(
    profile: &ProfileRoot,
    project_id: Option<String>,
    project_path: Option<String>,
    since: Option<String>,
    limit_sessions: usize,
    dry_run: bool,
) -> tracedecay_domain::errors::Result<()> {
    let project_root = resolve_cli_project_root(profile, None, project_id, project_path).await?;
    let since_ts = resolve_git_sync_since(since.as_deref())?;
    let outcome = session_sync_action(
        profile,
        &project_root,
        AdminCliSurfaceRequestV1::SessionsGitSync {
            since: since_ts,
            limit_sessions,
            dry_run,
        },
    )
    .await?;

    await_session_sync_completion(profile, &project_root, SessionSyncSurface::GitSync, outcome)
        .await?;
    if dry_run {
        println!("git-sync (dry-run): no rows were written");
    }
    Ok(())
}

pub(super) async fn run_sync_status(
    profile: &ProfileRoot,
    project_id: Option<String>,
    project_path: Option<String>,
    idempotency_key: String,
    json: bool,
) -> tracedecay_domain::errors::Result<()> {
    let project_root = resolve_cli_project_root(profile, None, project_id, project_path).await?;
    let outcome = session_sync_action(
        profile,
        &project_root,
        AdminCliSurfaceRequestV1::SessionsSyncStatus { idempotency_key },
    )
    .await?;
    println!("{}", sync_status_output(&project_root, outcome, json)?);
    Ok(())
}

fn sync_status_output(
    project_root: &Path,
    outcome: AdminCliSessionSyncV1,
    json: bool,
) -> tracedecay_domain::errors::Result<String> {
    let receipt = json.then(|| outcome.clone());
    let state = session_sync_poll_state(SessionSyncSurface::Status, outcome)?;
    if let Some(receipt) = receipt {
        return Ok(serde_json::to_string_pretty(&receipt)?);
    }
    Ok(match state {
        SessionSyncPollState::Pending { operation_id, .. } => format!(
            "session sync is still running ({}); no cancellation was requested",
            operation_id.as_str()
        ),
        SessionSyncPollState::Deferred {
            operation_id,
            idempotency_key,
            remaining_work,
        } => session_sync_deferred_report(
            project_root,
            "session sync",
            operation_id.as_str(),
            idempotency_key.as_str(),
            remaining_work,
        ),
        SessionSyncPollState::Completed { operation_id } => {
            format!("session sync completed ({})", operation_id.as_str())
        }
    })
}

#[derive(Debug)]
pub(super) enum SessionSyncPollState {
    Pending {
        operation_id: RequestId,
        idempotency_key: IdempotencyKey,
    },
    /// The operation finished and handed its remaining catch-up to the
    /// background refresh workers.
    Deferred {
        operation_id: RequestId,
        idempotency_key: IdempotencyKey,
        remaining_work: u64,
    },
    Completed {
        operation_id: RequestId,
    },
}

/// CLI surface observing a session sync operation.
#[derive(Clone, Copy, Debug)]
pub(super) enum SessionSyncSurface {
    Import,
    GitSync,
    Status,
}

impl SessionSyncSurface {
    fn label(self) -> &'static str {
        match self {
            Self::Import => "session import",
            Self::GitSync => "session git sync",
            Self::Status => "session sync",
        }
    }
}

fn sync_failed(
    label: &str,
    reason_code: &str,
    retryable: bool,
    detail: &str,
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::project_route(
        reason_code,
        retryable,
        format!("{label} did not complete successfully ({detail})"),
    )
}

pub(super) fn session_sync_poll_state(
    surface: SessionSyncSurface,
    outcome: AdminCliSessionSyncV1,
) -> tracedecay_domain::errors::Result<SessionSyncPollState> {
    let label = surface.label();
    match outcome {
        AdminCliSessionSyncV1::Accepted {
            operation_id,
            idempotency_key,
            ..
        }
        | AdminCliSessionSyncV1::Joined {
            operation_id,
            idempotency_key,
            ..
        } => Ok(SessionSyncPollState::Pending {
            operation_id,
            idempotency_key,
        }),
        AdminCliSessionSyncV1::Complete {
            operation_id,
            idempotency_key,
            source,
            termination,
            coverage,
            failure_codes,
            ..
        } => {
            let remaining_work = session_sync_remaining_work(&coverage).ok_or_else(|| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "daemon {label} response reported complete without truthful source coverage"
                    ),
                }
            })?;
            if source == SessionSyncSourceV1::ImportTranscripts
                && deferred_catch_up(termination, &coverage, &failure_codes, remaining_work)
            {
                return Ok(SessionSyncPollState::Deferred {
                    operation_id,
                    idempotency_key,
                    remaining_work,
                });
            }
            if termination != OperationTermination::Completed || remaining_work > 0 {
                let reason_code = if termination == OperationTermination::Completed {
                    "session_sync_incomplete".to_owned()
                } else {
                    format!("session_sync_{}", termination_label(termination))
                };
                let termination_kind = termination;
                let termination = termination_label(termination);
                let detail = if failure_codes.is_empty() {
                    termination
                } else {
                    format!("{termination}: {}", failure_codes.join(", "))
                };
                let detail = if remaining_work == 0 {
                    detail
                } else {
                    format!("{detail}; remaining work {remaining_work}")
                };
                return Err(sync_failed(
                    label,
                    &reason_code,
                    termination_retryable(termination_kind, remaining_work),
                    &detail,
                ));
            }
            Ok(SessionSyncPollState::Completed { operation_id })
        }
        AdminCliSessionSyncV1::Cancelled => Err(sync_failed(
            label,
            tracedecay_mcp::tool_errors::SESSION_SYNC_CANCELLED_BEFORE_ADMISSION,
            false,
            "cancelled",
        )),
        AdminCliSessionSyncV1::DeadlineExceeded => Err(sync_failed(
            label,
            tracedecay_mcp::tool_errors::SESSION_SYNC_DEADLINE_EXCEEDED,
            true,
            "deadline_exceeded",
        )),
        AdminCliSessionSyncV1::WrongScope => Err(sync_failed(
            label,
            "session_sync_wrong_scope",
            false,
            "wrong_scope",
        )),
        AdminCliSessionSyncV1::Unavailable { reason_code } => {
            Err(tracedecay_domain::errors::TraceDecayError::project_route(
                &reason_code,
                true,
                format!("{label} unavailable ({reason_code})"),
            ))
        }
    }
}

/// Wire (snake_case) spelling of a termination for report text.
fn termination_label(termination: OperationTermination) -> String {
    match serde_json::to_value(termination) {
        Ok(serde_json::Value::String(label)) => label,
        _ => format!("{termination:?}"),
    }
}

/// Whether rerunning the same sync can make progress. The owner ends a pass
/// `Partial` after committing progress with coverage left and `TimedOut` when
/// its deadline elapses, so a rerun resumes; it ends `Failed` only when
/// nothing committed, a caller's `Cancelled` stands, and `EffectUnknown`
/// cannot prove a rerun safe.
fn termination_retryable(termination: OperationTermination, remaining_work: u64) -> bool {
    match termination {
        OperationTermination::Completed | OperationTermination::Partial => remaining_work > 0,
        OperationTermination::TimedOut | OperationTermination::Unavailable => true,
        OperationTermination::Failed
        | OperationTermination::Cancelled
        | OperationTermination::EffectUnknown => false,
    }
}

fn deferred_catch_up(
    termination: OperationTermination,
    coverage: &[SessionSyncSourceCoverageV1],
    failure_codes: &[String],
    remaining_work: u64,
) -> bool {
    termination == OperationTermination::Partial
        && failure_codes.is_empty()
        && remaining_work > 0
        && coverage.iter().all(|entry| {
            matches!(
                entry.coverage,
                SessionSyncCoverageV1::Complete | SessionSyncCoverageV1::Partial { .. }
            )
        })
}

fn session_sync_remaining_work(coverage: &[SessionSyncSourceCoverageV1]) -> Option<u64> {
    if coverage.is_empty() {
        return None;
    }
    Some(coverage.iter().fold(0_u64, |remaining, entry| {
        remaining.saturating_add(entry.coverage.remaining_work())
    }))
}

pub(super) async fn await_session_sync_completion(
    profile: &ProfileRoot,
    project_root: &Path,
    surface: SessionSyncSurface,
    mut outcome: AdminCliSessionSyncV1,
) -> tracedecay_domain::errors::Result<()> {
    let label = surface.label();
    let client_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(35);
    // Poll with exponential backoff so a long-running sync costs dozens of
    // daemon round trips instead of one every 50 ms for up to 35 s.
    let mut poll_interval = std::time::Duration::from_millis(50);
    const MAX_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
    loop {
        match session_sync_poll_state(surface, outcome)? {
            SessionSyncPollState::Completed { operation_id } => {
                println!("{label} completed ({})", operation_id.as_str());
                return Ok(());
            }
            SessionSyncPollState::Deferred {
                operation_id,
                idempotency_key,
                remaining_work,
            } => {
                println!(
                    "{}",
                    session_sync_deferred_report(
                        project_root,
                        label,
                        operation_id.as_str(),
                        idempotency_key.as_str(),
                        remaining_work,
                    )
                );
                return Ok(());
            }
            SessionSyncPollState::Pending {
                operation_id,
                idempotency_key,
            } => {
                if tokio::time::Instant::now() >= client_deadline {
                    return Err(tracedecay_domain::errors::TraceDecayError::Config {
                        message: session_sync_timeout_message(
                            project_root,
                            label,
                            operation_id.as_str(),
                            idempotency_key.as_str(),
                        ),
                    });
                }
                tokio::time::sleep(poll_interval).await;
                poll_interval = (poll_interval * 2).min(MAX_POLL_INTERVAL);
                outcome = session_sync_action(
                    profile,
                    project_root,
                    AdminCliSurfaceRequestV1::SessionsSyncStatus {
                        idempotency_key: idempotency_key.as_str().to_owned(),
                    },
                )
                .await?;
            }
        }
    }
}

fn session_sync_timeout_message(
    project_root: &Path,
    label: &str,
    operation_id: &str,
    idempotency_key: &str,
) -> String {
    format!(
        "{label} observation ended after 35 seconds; background status is unknown and no \
         cancellation was requested (operation {operation_id}). Check status with: {}",
        sync_status_command(project_root, idempotency_key)
    )
}

fn session_sync_deferred_report(
    project_root: &Path,
    label: &str,
    operation_id: &str,
    idempotency_key: &str,
    remaining_work: u64,
) -> String {
    format!(
        "{label} scheduled ({operation_id}); historical catch-up has remaining work \
         {remaining_work} and continues in the background. Check status with: {}",
        sync_status_command(project_root, idempotency_key)
    )
}

fn sync_status_command(project_root: &Path, idempotency_key: &str) -> String {
    let project_root = project_root.to_string_lossy();
    shell_words::join([
        "tracedecay",
        "sessions",
        "sync-status",
        "--idempotency-key",
        idempotency_key,
        "--project-path",
        project_root.as_ref(),
    ])
}

/// Resolves the `--since` argument (ISO-8601 or unix seconds) to a unix-second
/// lower bound, defaulting to 90 days before now when unset.
fn resolve_git_sync_since(since: Option<&str>) -> tracedecay_domain::errors::Result<i64> {
    let Some(raw) = since.map(str::trim).filter(|value| !value.is_empty()) else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        return Ok((now - GIT_SYNC_DEFAULT_WINDOW_SECS).max(0));
    };
    if let Ok(unix) = raw.parse::<i64>() {
        if unix >= 0 {
            return Ok(unix);
        }
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "--since must be >= 0".to_string(),
        });
    }
    tracedecay_runtime_core::timeutil::parse_rfc3339_timestamp(raw).ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "--since must be a non-negative Unix timestamp or ISO/RFC3339 string (got `{raw}`)"
            ),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tracedecay_contracts::retrieval::AdminCliSessionSyncV1;
    use tracedecay_contracts::session_sync::{
        SessionGitSyncV1, SessionSyncCommandV1, SessionSyncCompletionReceiptV1,
        SessionSyncCoverageV1, SessionSyncJournalStatusV1, SessionSyncJournalV1,
        SessionSyncRequestV1, SessionSyncScopeV1, SessionSyncSourceCoverageV1, SessionSyncSourceV1,
        SessionSyncStatsV1, SessionTranscriptImportV1,
    };
    use tracedecay_contracts::{
        CancellationSignal, Deadline, IdempotencyKey, OperationTermination, RequestId,
    };
    use tracedecay_domain::{ProjectId, UserProfileId, UtcMicros};

    use super::{
        SessionSyncPollState, SessionSyncSurface, session_sync_deferred_report,
        session_sync_poll_state, session_sync_timeout_message, sync_status_output,
    };

    fn complete(
        termination: OperationTermination,
        coverage: Vec<SessionSyncCoverageV1>,
        failure_codes: &[&str],
    ) -> AdminCliSessionSyncV1 {
        AdminCliSessionSyncV1::Complete {
            operation_id: RequestId::new("operation.fixture").unwrap(),
            idempotency_key: IdempotencyKey::new("session-sync.fixture").unwrap(),
            source: SessionSyncSourceV1::ImportTranscripts,
            coalesced_primary: None,
            termination,
            stats: SessionSyncStatsV1::default(),
            coverage: coverage
                .into_iter()
                .map(|coverage| SessionSyncSourceCoverageV1 {
                    store_scope: "project".to_owned(),
                    coverage,
                })
                .collect(),
            source_frontiers: Vec::new(),
            failure_codes: failure_codes
                .iter()
                .map(|code| (*code).to_owned())
                .collect(),
            completed_at: UtcMicros(2),
        }
    }

    #[test]
    fn session_sync_timeout_preserves_the_resume_key_and_scope_without_claiming_cancellation() {
        let message = session_sync_timeout_message(
            Path::new("/repo/it's an example"),
            "session import",
            "operation.fixture",
            "session-sync.fixture's key",
        );

        assert!(message.contains("background status is unknown"));
        assert!(message.contains("no cancellation was requested"));
        let command = message.split_once("Check status with: ").unwrap().1;
        assert_eq!(
            shell_words::split(command).unwrap(),
            [
                "tracedecay",
                "sessions",
                "sync-status",
                "--idempotency-key",
                "session-sync.fixture's key",
                "--project-path",
                "/repo/it's an example",
            ]
        );
    }

    #[test]
    fn session_sync_admission_is_pending_until_truthful_completion() {
        assert!(matches!(
            session_sync_poll_state(
                SessionSyncSurface::Import,
                AdminCliSessionSyncV1::Accepted {
                    operation_id: RequestId::new("operation.fixture").unwrap(),
                    idempotency_key: IdempotencyKey::new("session-sync.fixture").unwrap(),
                    accepted_at: UtcMicros(1),
                }
            )
            .unwrap(),
            SessionSyncPollState::Pending { ref idempotency_key, .. }
                if idempotency_key.as_str() == "session-sync.fixture"
        ));
        assert!(matches!(
            session_sync_poll_state(
                SessionSyncSurface::Import,
                complete(
                    OperationTermination::Completed,
                    vec![SessionSyncCoverageV1::Complete],
                    &[]
                )
            )
            .unwrap(),
            SessionSyncPollState::Completed { .. }
        ));
    }

    #[test]
    fn session_sync_noncompletion_is_a_typed_route_refusal() {
        for (outcome, code, retryable, detail) in [
            (
                AdminCliSessionSyncV1::WrongScope,
                "session_sync_wrong_scope",
                false,
                "session import did not complete successfully (wrong_scope)",
            ),
            (
                AdminCliSessionSyncV1::DeadlineExceeded,
                "session_sync_deadline_exceeded",
                true,
                "session import did not complete successfully (deadline_exceeded)",
            ),
            (
                AdminCliSessionSyncV1::Cancelled,
                "session_sync_cancelled_before_admission",
                false,
                "session import did not complete successfully (cancelled)",
            ),
            (
                AdminCliSessionSyncV1::Unavailable {
                    reason_code: "session_sync_authority_unavailable".to_owned(),
                },
                "session_sync_authority_unavailable",
                true,
                "session import unavailable (session_sync_authority_unavailable)",
            ),
            (
                complete(
                    OperationTermination::Failed,
                    vec![SessionSyncCoverageV1::Complete],
                    &["native_transcript_scan_failed"],
                ),
                "session_sync_failed",
                false,
                "session import did not complete successfully (failed: native_transcript_scan_failed)",
            ),
            (
                complete(
                    OperationTermination::Partial,
                    vec![SessionSyncCoverageV1::Partial { deferred_units: 2 }],
                    &["native_transcript_scan_failed"],
                ),
                "session_sync_partial",
                true,
                "session import did not complete successfully (partial: native_transcript_scan_failed; remaining work 2)",
            ),
            (
                complete(
                    OperationTermination::Failed,
                    vec![SessionSyncCoverageV1::Partial { deferred_units: 2 }],
                    &["native_transcript_scan_failed"],
                ),
                "session_sync_failed",
                false,
                "session import did not complete successfully (failed: native_transcript_scan_failed; remaining work 2)",
            ),
            (
                complete(
                    OperationTermination::EffectUnknown,
                    vec![SessionSyncCoverageV1::Partial { deferred_units: 2 }],
                    &[],
                ),
                "session_sync_effect_unknown",
                false,
                "session import did not complete successfully (effect_unknown; remaining work 2)",
            ),
            (
                complete(
                    OperationTermination::TimedOut,
                    vec![SessionSyncCoverageV1::Complete],
                    &[],
                ),
                "session_sync_timed_out",
                true,
                "session import did not complete successfully (timed_out)",
            ),
            (
                complete(
                    OperationTermination::Completed,
                    vec![SessionSyncCoverageV1::Partial { deferred_units: 4 }],
                    &[],
                ),
                "session_sync_incomplete",
                true,
                "session import did not complete successfully (completed; remaining work 4)",
            ),
        ] {
            let error = session_sync_poll_state(SessionSyncSurface::Import, outcome).unwrap_err();
            assert_eq!(
                error.project_route_context(),
                Some((code, retryable, detail)),
                "{error}"
            );
        }
    }

    #[test]
    fn session_sync_refused_at_admission_renders_its_own_problem_kind() {
        for (outcome, kind) in [
            (AdminCliSessionSyncV1::Cancelled, "cancelled"),
            (AdminCliSessionSyncV1::DeadlineExceeded, "timed_out"),
        ] {
            let error = session_sync_poll_state(SessionSyncSurface::Import, outcome).unwrap_err();
            let document: serde_json::Value = serde_json::from_str(
                &tracedecay::mcp::tools::command_refusal_document(&error).unwrap(),
            )
            .unwrap();
            assert_eq!(document["problem"]["kind"], kind, "{document}");
            assert_eq!(
                document["problem"]["cancellation_stage"], "before_admission",
                "{document}"
            );
        }
    }

    #[test]
    fn session_import_accepts_deferred_catch_up_without_treating_it_as_failure() {
        let outcome = complete(
            OperationTermination::Partial,
            vec![SessionSyncCoverageV1::Partial { deferred_units: 1 }],
            &[],
        );

        assert!(matches!(
            session_sync_poll_state(SessionSyncSurface::Import, outcome).unwrap(),
            SessionSyncPollState::Deferred {
                remaining_work: 1,
                ..
            }
        ));
    }

    #[test]
    fn sync_status_reports_a_deferred_import_instead_of_failing() {
        let outcome = complete(
            OperationTermination::Partial,
            vec![
                SessionSyncCoverageV1::Partial { deferred_units: 1 },
                SessionSyncCoverageV1::Partial { deferred_units: 1 },
            ],
            &[],
        );

        assert!(matches!(
            session_sync_poll_state(SessionSyncSurface::Status, outcome).unwrap(),
            SessionSyncPollState::Deferred {
                remaining_work: 2,
                ..
            }
        ));
    }

    fn journaled_partial_receipt(command: SessionSyncCommandV1) -> AdminCliSessionSyncV1 {
        let request = SessionSyncRequestV1::new(
            RequestId::new("session-sync.fixture").unwrap(),
            IdempotencyKey::new("session-sync.fixture").unwrap(),
            SessionSyncScopeV1::new(
                ProjectId::new("project.fixture").unwrap(),
                UserProfileId::new("profile.fixture").unwrap(),
            ),
            Deadline::new(UtcMicros(200)).unwrap(),
            CancellationSignal::active("session-sync.fixture").unwrap(),
            command,
        );
        let mut journal = SessionSyncJournalV1::queued(&request, UtcMicros(10));
        journal.status = SessionSyncJournalStatusV1::Complete;
        journal.completion = Some(SessionSyncCompletionReceiptV1 {
            admission: journal.admission.clone(),
            coalesced_primary: None,
            completed_at: UtcMicros(20),
            termination: OperationTermination::Partial,
            stats: SessionSyncStatsV1::default(),
            coverage: vec![SessionSyncSourceCoverageV1 {
                store_scope: "git".to_owned(),
                coverage: SessionSyncCoverageV1::Partial { deferred_units: 1 },
            }],
            source_frontiers: Vec::new(),
            failure_codes: Vec::new(),
        });
        journal.outcome().into()
    }

    #[test]
    fn sync_status_rejects_a_git_sync_receipt_with_remaining_sessions() {
        let git_sync =
            SessionSyncCommandV1::SynchronizeGit(SessionGitSyncV1::new(0, 1, false).unwrap());
        for json in [false, true] {
            let error = sync_status_output(
                Path::new("/repo"),
                journaled_partial_receipt(git_sync),
                json,
            )
            .expect_err("git sync schedules no background catch-up");

            assert_eq!(
                error.project_route_context(),
                Some((
                    "session_sync_partial",
                    true,
                    "session sync did not complete successfully (partial; remaining work 1)"
                ))
            );
        }
    }

    #[test]
    fn sync_status_reports_a_journaled_deferred_import() {
        let import =
            SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts());

        let report =
            sync_status_output(Path::new("/repo"), journaled_partial_receipt(import), false)
                .unwrap();

        assert!(report.contains("continues in the background"));
    }

    #[test]
    fn deferred_import_report_names_the_sync_status_command_for_its_key() {
        let report = session_sync_deferred_report(
            Path::new("/repo/it's an example"),
            "session import",
            "operation.fixture",
            "session-sync.fixture",
            2,
        );

        assert!(report.starts_with("session import scheduled (operation.fixture);"));
        let command = report.split_once("Check status with: ").unwrap().1;
        assert_eq!(
            shell_words::split(command).unwrap(),
            [
                "tracedecay",
                "sessions",
                "sync-status",
                "--idempotency-key",
                "session-sync.fixture",
                "--project-path",
                "/repo/it's an example",
            ]
        );
    }

    #[test]
    fn session_git_sync_still_rejects_unfinished_coverage() {
        let git_sync =
            SessionSyncCommandV1::SynchronizeGit(SessionGitSyncV1::new(0, 1, false).unwrap());
        let error = session_sync_poll_state(
            SessionSyncSurface::GitSync,
            journaled_partial_receipt(git_sync),
        )
        .expect_err("git sync still requires the bounded pass to finish");

        assert_eq!(
            error.project_route_context(),
            Some((
                "session_sync_partial",
                true,
                "session git sync did not complete successfully (partial; remaining work 1)"
            ))
        );
    }

    #[test]
    fn session_sync_reports_remaining_coverage_even_if_daemon_mislabels_completion() {
        let error = session_sync_poll_state(
            SessionSyncSurface::Import,
            complete(
                OperationTermination::Completed,
                vec![SessionSyncCoverageV1::Partial { deferred_units: 4 }],
                &[],
            ),
        )
        .expect_err("partial transcript coverage cannot be CLI success");

        assert!(error.to_string().contains("remaining work 4"));
    }

    #[test]
    fn session_sync_rejects_completion_without_source_coverage() {
        let error = session_sync_poll_state(
            SessionSyncSurface::Import,
            complete(OperationTermination::Completed, Vec::new(), &[]),
        )
        .expect_err("coverage-free completion cannot prove convergence");

        assert!(
            error
                .to_string()
                .contains("without truthful source coverage")
        );
    }
}
