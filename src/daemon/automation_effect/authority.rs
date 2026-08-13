//! Blocking journal cleanup and retirement finalization for the authority facade.

use std::path::Path;

use super::{contract_error, journal::DurableAutomationAdmission, recovery_index, retirement};
use crate::errors::Result;

pub(super) async fn finalize_terminal_housekeeping(
    dashboard_root: &Path,
    journal_path: &Path,
    admission: DurableAutomationAdmission,
    retirement_binding: Option<retirement::RetirementBinding>,
    live_plan: Option<retirement::RetirementPlan>,
) -> Result<()> {
    let dashboard_root = dashboard_root.to_path_buf();
    let journal_path = journal_path.to_path_buf();
    spawn_terminal_housekeeping(
        {
            let dashboard_root = dashboard_root.clone();
            move || {
                retirement_binding
                    .map(|binding| {
                        retirement::finalize_after_terminal(
                            &dashboard_root,
                            &binding,
                            live_plan.as_ref(),
                        )
                    })
                    .transpose()
            }
        },
        {
            let dashboard_root = dashboard_root.clone();
            let journal_path = journal_path.clone();
            let admission = admission.clone();
            move |closure| {
                if let Some(closure) = closure {
                    recovery_index::remove_pending_for_retirement_blocking(
                        &dashboard_root,
                        &journal_path,
                        &admission,
                        closure,
                    )
                } else {
                    recovery_index::remove_pending_blocking(&dashboard_root, &journal_path)
                }
            }
        },
        {
            let dashboard_root = dashboard_root.clone();
            let journal_path = journal_path.clone();
            let admission = admission.clone();
            move |closure| {
                retirement::complete_after_pending_removal(&closure)?;
                recovery_index::finish_retirement_transition_blocking(
                    &dashboard_root,
                    &journal_path,
                    &admission,
                    &closure,
                )
            }
        },
        move || recovery_index::add_pending_blocking(&dashboard_root, &journal_path, &admission),
    )
    .await
    .map_err(|error| {
        contract_error(format!(
            "retained automation terminal housekeeping task failed: {error}"
        ))
    })?
}

fn spawn_terminal_housekeeping<T: Send + 'static>(
    finalize: impl FnOnce() -> Result<Option<T>> + Send + 'static,
    remove_pending: impl FnOnce(Option<&T>) -> Result<()> + Send + 'static,
    complete: impl FnOnce(T) -> Result<()> + Send + 'static,
    retain_pending: impl FnMut() -> Result<()> + Send + 'static,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::task::spawn_blocking(move || {
        run_terminal_housekeeping(finalize, remove_pending, complete, retain_pending)
    })
}

fn run_terminal_housekeeping<T>(
    finalize: impl FnOnce() -> Result<Option<T>>,
    remove_pending: impl FnOnce(Option<&T>) -> Result<()>,
    complete: impl FnOnce(T) -> Result<()>,
    mut retain_pending: impl FnMut() -> Result<()>,
) -> Result<()> {
    let closure = finalize()?;
    if let Err(error) = remove_pending(closure.as_ref()) {
        return retain_after_error(error, &mut retain_pending);
    }
    if let Some(closure) = closure
        && let Err(error) = complete(closure)
    {
        return retain_after_error(error, &mut retain_pending);
    }
    Ok(())
}

fn retain_after_error(
    error: crate::errors::TraceDecayError,
    retain_pending: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    match retain_pending() {
        Ok(()) => Err(error),
        Err(retain_error) => Err(contract_error(format!(
            "{error}; additionally failed to retain retirement recovery authority: {retain_error}"
        ))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    async fn receive_boundary(receiver: std::sync::mpsc::Receiver<()>, label: &'static str) {
        tokio::task::spawn_blocking(move || receiver.recv().expect(label))
            .await
            .expect(label);
    }

    #[tokio::test]
    async fn abort_after_pending_removal_cannot_skip_witness_completion() {
        let (removed_sender, removed_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let (completed_sender, completed_receiver) = std::sync::mpsc::channel();
        let retained = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let retained_for_owner = std::sync::Arc::clone(&retained);
        let caller = tokio::spawn(async move {
            spawn_terminal_housekeeping(
                || Ok(Some(())),
                move |_| {
                    removed_sender.send(()).expect("removed signal");
                    release_receiver.recv().expect("remove release");
                    Ok(())
                },
                move |()| {
                    completed_sender.send(()).expect("completion signal");
                    Ok(())
                },
                move || {
                    retained_for_owner.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                },
            )
            .await
            .expect("blocking owner join")
        });

        receive_boundary(removed_receiver, "pending removal boundary").await;
        caller.abort();
        release_sender.send(()).expect("release pending removal");
        receive_boundary(completed_receiver, "witness completion").await;

        assert!(caller.await.expect_err("caller was aborted").is_cancelled());
        assert_eq!(retained.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn abort_during_failed_witness_completion_still_restores_pending_authority() {
        let (completion_sender, completion_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let (retained_sender, retained_receiver) = std::sync::mpsc::channel();
        let caller = tokio::spawn(async move {
            spawn_terminal_housekeeping(
                || Ok(Some(())),
                |_| Ok(()),
                move |()| {
                    completion_sender.send(()).expect("completion boundary");
                    release_receiver.recv().expect("completion release");
                    Err(contract_error("injected witness completion failure"))
                },
                move || {
                    retained_sender.send(()).expect("pending retention signal");
                    Ok(())
                },
            )
            .await
            .expect("blocking owner join")
        });

        receive_boundary(completion_receiver, "witness completion boundary").await;
        caller.abort();
        release_sender.send(()).expect("release witness completion");
        receive_boundary(retained_receiver, "pending authority restoration").await;

        assert!(caller.await.expect_err("caller was aborted").is_cancelled());
    }
}
