use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use tracedecay_automation_runtime::automation::AutomationRunControl;

use crate::tracedecay::TraceDecay;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{
    DaemonEngine, DaemonHandshake, effective_automation_config_for_project, log_daemon_event,
};

const HOST_RECEIPT_REVIEW_BATCH_LIMIT: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostReceiptReviewProgress {
    Completed,
    Deferred,
    Idle,
}

#[hotpath::measure(label = "daemon.scheduler.host_receipt_review", future = true)]
pub(super) async fn run_host_receipt_review(
    project_path: &Path,
    cg: &TraceDecay,
    handshake: &DaemonHandshake,
    engine: &DaemonEngine,
    run_control: &AutomationRunControl,
) -> Result<()> {
    run_host_receipt_review_inner(project_path, cg, handshake, engine, run_control).await
}

/// The review pass behind [`run_host_receipt_review`], boxed at definition so
/// the instrumented outer future stays a pointer-sized state machine and each
/// per-receipt review (which inlines combined-effect preparation and
/// execution) lives on the heap rather than in one scheduler poll frame.
fn run_host_receipt_review_inner<'a>(
    project_path: &'a Path,
    cg: &'a TraceDecay,
    handshake: &'a DaemonHandshake,
    engine: &'a DaemonEngine,
    run_control: &'a AutomationRunControl,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        drain_ready_host_receipts(|| {
            boxed_one_host_receipt_review(project_path, cg, handshake, engine, run_control)
        })
        .await
        .map(|_| ())
    })
}

/// One receipt review as a type-erased boxed future, so neither the drain
/// loop's state machine nor any layout query above it names the concrete
/// review future.
fn boxed_one_host_receipt_review<'a>(
    project_path: &'a Path,
    cg: &'a TraceDecay,
    handshake: &'a DaemonHandshake,
    engine: &'a DaemonEngine,
    run_control: &'a AutomationRunControl,
) -> Pin<Box<dyn Future<Output = Result<HostReceiptReviewProgress>> + Send + 'a>> {
    Box::pin(run_one_host_receipt_review(
        project_path,
        cg,
        handshake,
        engine,
        run_control,
    ))
}

async fn drain_ready_host_receipts<Review, ReviewFuture>(mut review: Review) -> Result<usize>
where
    Review: FnMut() -> ReviewFuture,
    ReviewFuture: Future<Output = Result<HostReceiptReviewProgress>>,
{
    let mut completed = 0;
    while completed < HOST_RECEIPT_REVIEW_BATCH_LIMIT {
        match review().await? {
            HostReceiptReviewProgress::Completed => completed += 1,
            HostReceiptReviewProgress::Deferred | HostReceiptReviewProgress::Idle => break,
        }
    }
    Ok(completed)
}

async fn run_one_host_receipt_review(
    project_path: &Path,
    cg: &TraceDecay,
    _handshake: &DaemonHandshake,
    engine: &DaemonEngine,
    run_control: &AutomationRunControl,
) -> Result<HostReceiptReviewProgress> {
    use tracedecay_automation_runtime::automation::backend::CodexAppServerBackend;
    use tracedecay_automation_runtime::automation::run_ledger::AutomationTrigger;
    use tracedecay_automation_runtime::automation::runner::{
        CombinedReviewAutomationOptions, SessionReflectorAutomationOptions,
        SkillWriterAutomationOptions, registered_project_automation_retrieval,
    };

    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let Some(ready) =
        tracedecay_automation_runtime::automation::host_receipts::oldest_ready(&dashboard_root)
            .await?
    else {
        return Ok(HostReceiptReviewProgress::Idle);
    };
    let pending = ready.pending;
    if tracedecay_automation_runtime::automation::scheduler::load_scheduler_control(&dashboard_root)
        .await?
        .paused
    {
        return Ok(HostReceiptReviewProgress::Deferred);
    }
    let configuration = effective_automation_config_for_project(cg).await?;
    let config = &configuration.settings;
    let session_id = pending
        .route
        .as_ref()
        .and_then(|route| route.session_id.clone());
    let Some(authoritative_project_id) = cg.store_layout().identity.project_id.as_deref() else {
        return Ok(HostReceiptReviewProgress::Deferred);
    };
    let project_id = tracedecay_domain::ProjectId::new(authoritative_project_id.to_string())
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "host receipt review has an invalid authoritative project identity: {error}"
            ),
        })?;
    let session_database = engine
        .store_administration
        .registered_project_session_database(project_path, cg.store_layout())
        .await?;
    let watermark_durable =
        {
            let snapshot = session_database.read_snapshot().await.map_err(|error| {
                TraceDecayError::Config {
                    message: format!("host receipt session snapshot unavailable: {error}"),
                }
            })?;
            let mut rows = snapshot
                .query(
                    "SELECT 1
                 FROM lcm_raw_messages
                 WHERE provider = ?1 AND message_id = ?2
                 LIMIT 1",
                    tracedecay_runtime_core::db::engine::params![
                        "hermes",
                        ready.transcript_watermark.as_str()
                    ],
                )
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("host receipt transcript watermark query failed: {error}"),
                })?;
            rows.next()
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("host receipt transcript watermark read failed: {error}"),
                })?
                .is_some()
        };
    if !watermark_durable {
        // Never review a terminal receipt until the exact completed-turn
        // watermark is durable in LCM.
        return Ok(HostReceiptReviewProgress::Deferred);
    }
    let profile_identity = engine.store_administration.profile_identity()?.clone();
    let retrieval =
        registered_project_automation_retrieval(session_database, &profile_identity, &project_id)
            .await?;
    let backend = CodexAppServerBackend::from_automation_config(config);
    let host_run_id = format!("host_receipt_{}", pending.generation);
    let combined_options = CombinedReviewAutomationOptions {
        session_reflector: SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::HostReceipt,
            provider: "hermes".to_string(),
            session_id,
            ..SessionReflectorAutomationOptions::default()
        },
        skill_writer: SkillWriterAutomationOptions {
            trigger: AutomationTrigger::HostReceipt,
            provider: "hermes".to_string(),
            profile_root: Some(profile_identity.profile_root().to_path_buf()),
            ..SkillWriterAutomationOptions::default()
        },
        trigger: AutomationTrigger::HostReceipt,
        ..CombinedReviewAutomationOptions::default()
    };
    let admission = Box::pin(super::combined_effect::prepare_combined_effects(
        engine,
        cg,
        run_control,
        project_path,
        &dashboard_root,
        Some(&host_run_id),
        configuration.configuration_digest.clone(),
        &combined_options,
    ))
    .await?;
    let mut first_error = None;
    let outcome = Box::pin(super::combined_effect::run_combined_scheduler_effect(
        admission,
        engine,
        cg,
        &project_id,
        project_path,
        config,
        &configuration.configuration_revision_id,
        &backend,
        retrieval.as_ref(),
        combined_options,
        &mut first_error,
    ))
    .await;
    if let Some(error) = first_error {
        return Err(error);
    }
    if outcome.completed() {
        tracedecay_automation_runtime::automation::host_receipts::mark_consumed(
            &dashboard_root,
            &pending.session_key,
            pending.generation,
        )
        .await?;
        Ok(HostReceiptReviewProgress::Completed)
    } else if !outcome.handled() {
        log_daemon_event(
            "host_receipt_review",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "deferred".to_string()),
                ("reason", "not_combined".to_string()),
            ],
        );
        Ok(HostReceiptReviewProgress::Deferred)
    } else {
        Ok(HostReceiptReviewProgress::Deferred)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        HOST_RECEIPT_REVIEW_BATCH_LIMIT, HostReceiptReviewProgress, drain_ready_host_receipts,
    };

    #[tokio::test]
    async fn one_review_pass_drains_multiple_ready_receipts() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);

        let completed = drain_ready_host_receipts(move || {
            let call = observed.fetch_add(1, Ordering::SeqCst);
            async move {
                Ok(if call < 3 {
                    HostReceiptReviewProgress::Completed
                } else {
                    HostReceiptReviewProgress::Idle
                })
            }
        })
        .await
        .expect("drain ready receipts");

        assert_eq!(completed, 3);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "the pass should fetch the next receipt without returning to fixed tasks"
        );
    }

    #[tokio::test]
    async fn receipt_review_drain_stops_at_its_batch_bound() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);

        let completed = drain_ready_host_receipts(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async { Ok(HostReceiptReviewProgress::Completed) }
        })
        .await
        .expect("drain bounded receipt batch");

        assert_eq!(completed, HOST_RECEIPT_REVIEW_BATCH_LIMIT);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            HOST_RECEIPT_REVIEW_BATCH_LIMIT
        );
    }
}
