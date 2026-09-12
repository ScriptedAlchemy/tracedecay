use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use tracedecay_automation_runtime::automation::AutomationRunControl;

use crate::project::TraceDecay;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{DaemonEngine, DaemonHandshake, effective_automation_config_for_project};
use tracedecay_runtime_core::logging::log_daemon_event;

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

/// A terminal host receipt is never reviewed until its exact completed-turn
/// watermark is durable in LCM: an absent watermark defers the review, and an
/// unreadable snapshot is a typed error, never a pass.
async fn transcript_watermark_is_durable(
    session_database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    watermark: &str,
) -> Result<bool> {
    let snapshot =
        session_database
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("host receipt session snapshot unavailable: {error}"),
            })?;
    let mut rows = snapshot
        .query(
            "SELECT 1
                 FROM lcm_raw_messages
                 WHERE provider = ?1 AND message_id = ?2
                 LIMIT 1",
            tracedecay_runtime_core::db::engine::params!["hermes", watermark],
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("host receipt transcript watermark query failed: {error}"),
        })?;
    Ok(rows
        .next()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("host receipt transcript watermark read failed: {error}"),
        })?
        .is_some())
}

#[expect(
    clippy::too_many_lines,
    reason = "Host-receipt review is one load-review-settle pass for a single receipt."
)]
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
    let automation_context = cg.automation_project_context()?;
    let session_id = pending
        .route
        .as_ref()
        .and_then(|route| route.session_id.clone());
    let session_database = engine
        .store_administration
        .registered_project_session_database(automation_context.project_root(), cg.store_layout())
        .await?;
    let watermark_durable =
        transcript_watermark_is_durable(&session_database, ready.transcript_watermark.as_str())
            .await?;
    if !watermark_durable {
        // Never review a terminal receipt until the exact completed-turn
        // watermark is durable in LCM.
        return Ok(HostReceiptReviewProgress::Deferred);
    }
    let profile_identity = engine.store_administration.profile_identity()?.clone();
    let retrieval = registered_project_automation_retrieval(
        session_database,
        &profile_identity,
        automation_context.project_id(),
    )
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
        automation_context.project_root(),
        &automation_context.dashboard_root,
        Some(&host_run_id),
        configuration.configuration_digest.clone(),
        &combined_options,
    ))
    .await?;
    let mut first_error = None;
    let outcome = Box::pin(super::combined_effect::run_combined_scheduler_effect(
        admission,
        engine,
        &automation_context,
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

    use tracedecay_automation_runtime::automation::AutomationRunControl;
    use tracedecay_daemon_protocol::{DaemonClientIdentity, DaemonHandshake, MovedStoreAdoption};
    use tracedecay_hooks::{HookRouteMetadata, HookTerminalReceipt};

    use super::{
        DaemonEngine, HOST_RECEIPT_REVIEW_BATCH_LIMIT, HostReceiptReviewProgress,
        drain_ready_host_receipts, run_one_host_receipt_review,
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

    #[tokio::test]
    async fn context_failure_precedes_host_receipt_admission() {
        let directory = tempfile::tempdir().expect("temporary project");
        let project_root = directory.path().join("project");
        let profile_root = directory.path().join("profile");
        std::fs::create_dir_all(project_root.join("src")).expect("project source directory");
        std::fs::write(project_root.join("src/lib.rs"), "pub fn fixture() {}\n")
            .expect("project source");
        let options = crate::project::TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        };
        let writable =
            crate::project::TraceDecay::init_with_options(&project_root, options.clone())
                .await
                .expect("initialize host receipt project");
        let dashboard_root = writable.store_layout().dashboard_root.clone();
        let route = Some(HookRouteMetadata {
            session_id: Some("session.context-failure".to_owned()),
            thread_id: None,
            cwd: None,
            worktree: None,
            branch: None,
        });
        tracedecay_automation_runtime::automation::host_receipts::record(
            &dashboard_root,
            route.clone(),
            HookTerminalReceipt {
                tool_call_id: Some("call.context-failure".to_owned()),
                turn_id: Some("turn.context-failure".to_owned()),
                status: Some("success".to_owned()),
                duration_ms: Some(1),
                transcript_watermark: Some("message.context-failure".to_owned()),
            },
        )
        .await
        .expect("record host receipt");
        tracedecay_automation_runtime::automation::host_receipts::mark_turn_ingested(
            &dashboard_root,
            route,
            "message.context-failure",
        )
        .await
        .expect("mark host receipt ready");
        writable.close();
        let read_only =
            crate::project::TraceDecay::open_read_only_with_options(&project_root, options)
                .await
                .expect("open read-only host receipt project");
        let handshake = DaemonHandshake {
            project_path: Some(project_root.clone()),
            scope_prefix: None,
            timings: false,
            allow_init: false,
            allow_initialize_root_routing: false,
            client_identity: DaemonClientIdentity::new(
                profile_root.clone(),
                profile_root.join("global.db"),
            ),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            client_instance_id: "client.context-failure".to_owned(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
            moved_store_adoption: MovedStoreAdoption::Never,
        };

        let error = run_one_host_receipt_review(
            &project_root,
            &read_only,
            &handshake,
            &DaemonEngine::default(),
            &AutomationRunControl::from_interrupted(Arc::new(|| false)),
        )
        .await
        .expect_err("read-only automation context must fail before admission");

        assert!(
            error.to_string().contains("open read-only"),
            "context failure must win over admission: {error}"
        );
        assert!(
            !dashboard_root.join("automation_effects").exists(),
            "context failure must not leave a durable automation reservation"
        );
    }
}
