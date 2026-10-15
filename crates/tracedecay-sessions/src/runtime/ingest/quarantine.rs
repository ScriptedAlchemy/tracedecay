use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use sha2::{Digest, Sha256};
use tracedecay_runtime_core::resident_memory::ProcessSharedMemoryReservationV1;

use super::failure::{ProviderRunOutcome, TranscriptCatchUpFailure};
use crate::runtime::hosts::codex::{
    CodexDiscoveryHub, candidate_charge, change_stamp, codex_corpus_identity,
};
use crate::runtime::jsonl_observation_admission::reserve_shared_jsonl_bytes;
use crate::runtime::source::{
    TranscriptIngestError, TranscriptIngestResult, run_blocking_transcript_section,
};
use crate::runtime::{classify_transcript_ingest_disposition, classify_transcript_ingest_failure};

#[derive(Clone, Default)]
pub(crate) struct SourceLedger {
    inner: Arc<Mutex<HashMap<SourceKey, Disposition>>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SourceKey {
    origin: PathBuf,
    stream: Option<Box<str>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Witness {
    identity: [u8; 32],
    len: u64,
}

struct Disposition {
    witness: Witness,
    quarantined: bool,
    _memory: ProcessSharedMemoryReservationV1,
}

impl SourceKey {
    pub(crate) fn file(path: &Path) -> Self {
        Self {
            origin: path.to_path_buf(),
            stream: None,
        }
    }
}

impl SourceLedger {
    pub(crate) fn holds(&self, key: &SourceKey, witness: Witness) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .is_some_and(|recorded| recorded.witness == witness)
    }

    fn recorded_converged(&self, key: &SourceKey) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .is_some_and(|recorded| !recorded.quarantined)
    }

    fn record(
        &self,
        key: &SourceKey,
        witness: Witness,
        quarantined: bool,
    ) -> TranscriptIngestResult<()> {
        let mut records = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(recorded) = records.get_mut(key) {
            recorded.witness = witness;
            recorded.quarantined = quarantined;
            return Ok(());
        }
        let stream_len = key.stream.as_deref().map_or(0, str::len);
        let charge = candidate_charge(
            &key.origin,
            u64::try_from(std::mem::size_of::<(SourceKey, Disposition)>() + stream_len)
                .unwrap_or(u64::MAX),
        )?;
        let Some(reservation) =
            reserve_shared_jsonl_bytes(charge, "transcript read outcome capacity")?
        else {
            return Ok(());
        };
        records.insert(
            key.clone(),
            Disposition {
                witness,
                quarantined,
                _memory: reservation,
            },
        );
        Ok(())
    }
}

impl Witness {
    pub(crate) fn settled_file(
        path: &Path,
        metadata: &std::fs::Metadata,
    ) -> TranscriptIngestResult<Option<Self>> {
        let changed = change_stamp(path, metadata);
        if !metadata.is_file() || !changed.is_settled() {
            return Ok(None);
        }
        Ok(Some(Self {
            identity: codex_corpus_identity(path, metadata, changed)?,
            len: metadata.len(),
        }))
    }

    fn content(parts: &[&[u8]]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"tracedecay-source-content-witness-v1");
        let mut len = 0u64;
        for part in parts {
            let part_len = u64::try_from(part.len()).unwrap_or(u64::MAX);
            hasher.update(part_len.to_le_bytes());
            hasher.update(part);
            len = len.saturating_add(part_len);
        }
        Self {
            identity: hasher.finalize().into(),
            len,
        }
    }
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the snapshot providers adopt multi-file witnesses"
    )
)]
impl Witness {
    fn settled_files(members: &[PathBuf]) -> TranscriptIngestResult<Option<Self>> {
        let mut hasher = Sha256::new();
        hasher.update(b"tracedecay-source-member-files-witness-v1");
        let mut len = 0u64;
        for member in members {
            match stat_if_present(member, "stat transcript source member")? {
                Some(metadata) => {
                    let Some(witness) = Self::settled_file(member, &metadata)? else {
                        return Ok(None);
                    };
                    hasher.update([1]);
                    hasher.update(witness.identity);
                    len = len.saturating_add(witness.len);
                }
                None => {
                    let name = member.as_os_str().as_encoded_bytes();
                    hasher.update([0]);
                    hasher.update(u64::try_from(name.len()).unwrap_or(u64::MAX).to_le_bytes());
                    hasher.update(name);
                }
            }
        }
        Ok(Some(Self {
            identity: hasher.finalize().into(),
            len,
        }))
    }
}

pub(crate) struct SourceClaim {
    key: SourceKey,
    settled: Option<(SourceLedger, Witness)>,
}

impl SourceClaim {
    fn unless_recorded(
        ledger: Option<&SourceLedger>,
        key: SourceKey,
        witness: Option<Witness>,
    ) -> Option<Self> {
        if let Some(ledger) = ledger {
            let recorded = match witness {
                Some(witness) => ledger.holds(&key, witness),
                None => ledger.recorded_converged(&key),
            };
            if recorded {
                return None;
            }
        }
        Some(Self {
            key,
            settled: ledger.cloned().zip(witness),
        })
    }

    pub(crate) async fn observe_file(
        provider: &'static str,
        ledger: Option<SourceLedger>,
        path: &Path,
    ) -> TranscriptIngestResult<Option<Self>> {
        let path = path.to_path_buf();
        run_blocking_transcript_section(provider, move || {
            Self::observe_file_blocking(ledger.as_ref(), &path)
        })
        .await?
    }

    pub(crate) fn observe_file_blocking(
        ledger: Option<&SourceLedger>,
        path: &Path,
    ) -> TranscriptIngestResult<Option<Self>> {
        let key = SourceKey::file(path);
        if ledger.is_none() {
            return Ok(Some(Self { key, settled: None }));
        }
        // Codex archives a session by moving its rollout away; failing on the
        // vanished path would replay the unacknowledged discovery pass forever.
        let Some(metadata) = stat_if_present(path, "stat transcript for convergence")? else {
            return Ok(None);
        };
        let witness = Witness::settled_file(path, &metadata)?;
        Ok(Self::unless_recorded(ledger, key, witness))
    }

    pub(crate) fn admitted(
        self,
        source_deferred: bool,
        covered_through: u64,
    ) -> TranscriptIngestResult<()> {
        if self.admission_settles(source_deferred, covered_through) {
            self.finished()
        } else {
            Ok(())
        }
    }

    pub(crate) fn finished(self) -> TranscriptIngestResult<()> {
        match self.settled {
            Some((ledger, witness)) => ledger.record(&self.key, witness, false),
            None => Ok(()),
        }
    }

    pub(crate) fn admission_settles(&self, source_deferred: bool, covered_through: u64) -> bool {
        self.settled
            .as_ref()
            .is_some_and(|(_, witness)| !source_deferred && covered_through == witness.len)
    }

    pub(crate) fn settled(&self) -> Option<SettledClaim> {
        self.settled.as_ref().map(|(ledger, witness)| SettledClaim {
            ledger: ledger.clone(),
            key: self.key.clone(),
            witness: *witness,
        })
    }
}

pub(crate) struct SettledClaim {
    ledger: SourceLedger,
    key: SourceKey,
    witness: Witness,
}

impl SettledClaim {
    pub(crate) fn finished(self) -> TranscriptIngestResult<()> {
        self.ledger.record(&self.key, self.witness, false)
    }
}

pub(crate) trait SourceFailure {
    fn catch_up_failure(&self, provider: &'static str) -> (TranscriptCatchUpFailure, bool);
}

impl SourceFailure for TranscriptIngestError {
    fn catch_up_failure(&self, provider: &'static str) -> (TranscriptCatchUpFailure, bool) {
        (
            classify_transcript_ingest_failure(provider, "observation", self),
            classify_transcript_ingest_disposition(self).quarantines_source(),
        )
    }
}

pub(crate) struct SourceSweep {
    provider: &'static str,
    ledger: Option<SourceLedger>,
    blocked: HashSet<SourceKey>,
    failures: Vec<TranscriptCatchUpFailure>,
}

impl SourceSweep {
    pub(crate) fn new(
        provider: &'static str,
        discovery: Option<(&CodexDiscoveryHub, &str)>,
    ) -> Self {
        Self {
            provider,
            ledger: discovery.and_then(|(hub, consumer)| hub.source_ledger(consumer)),
            blocked: HashSet::new(),
            failures: Vec::new(),
        }
    }

    pub(crate) async fn observe_file(
        &self,
        path: &Path,
    ) -> TranscriptIngestResult<Option<SourceClaim>> {
        if self.blocked.contains(&SourceKey::file(path)) {
            return Ok(None);
        }
        SourceClaim::observe_file(self.provider, self.ledger.clone(), path).await
    }

    pub(crate) fn refused<E: SourceFailure>(
        &mut self,
        claim: SourceClaim,
        error: E,
    ) -> Result<(), E> {
        let (failure, quarantines) = error.catch_up_failure(self.provider);
        // A sweep without a ledger cannot remember the quarantine, so its
        // caller (a hook ingesting one session) keeps reporting the error.
        if !quarantines || self.ledger.is_none() {
            return Err(error);
        }
        if let Some((ledger, witness)) = &claim.settled
            && let Err(record_error) = ledger.record(&claim.key, *witness, true)
        {
            tracing::warn!(
                provider = self.provider,
                error = %record_error,
                "transcript source quarantine was not recorded"
            );
        }
        tracing::warn!(
            provider = self.provider,
            reason_code = failure.reason_code,
            "transcript source permanently refused; skipped until it changes"
        );
        self.failures.push(failure);
        self.blocked.insert(claim.key);
        Ok(())
    }

    pub(crate) fn report_into(self, outcome: &mut ProviderRunOutcome) {
        outcome.failures.splice(0..0, self.failures);
    }

    pub(crate) fn observe_stream(
        &mut self,
        container: &Path,
        stream: &str,
        content: &[&[u8]],
    ) -> Option<SourceClaim> {
        let key = SourceKey {
            origin: container.to_path_buf(),
            stream: Some(stream.into()),
        };
        if self.blocked.contains(&key) {
            return None;
        }
        let witness = Witness::content(content);
        if self
            .ledger
            .as_ref()
            .is_some_and(|ledger| ledger.holds(&key, witness))
        {
            self.blocked.insert(key);
            return None;
        }
        Some(SourceClaim {
            settled: self.ledger.clone().map(|ledger| (ledger, witness)),
            key,
        })
    }
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the snapshot providers adopt these observations")
)]
impl SourceSweep {
    #[expect(
        dead_code,
        reason = "the JSONL providers observe through it on their blocking threads"
    )]
    pub(crate) fn ledger(&self) -> Option<SourceLedger> {
        self.ledger.clone()
    }

    pub(crate) async fn observe_files(
        &self,
        key: &Path,
        members: Vec<PathBuf>,
    ) -> TranscriptIngestResult<Option<SourceClaim>> {
        let key = SourceKey::file(key);
        if self.blocked.contains(&key) {
            return Ok(None);
        }
        let Some(ledger) = self.ledger.clone() else {
            return Ok(Some(SourceClaim { key, settled: None }));
        };
        run_blocking_transcript_section(self.provider, move || {
            let witness = Witness::settled_files(&members)?;
            Ok(SourceClaim::unless_recorded(Some(&ledger), key, witness))
        })
        .await?
    }
}

pub(crate) fn stat_if_present(
    path: &Path,
    operation: &'static str,
) -> TranscriptIngestResult<Option<std::fs::Metadata>> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(TranscriptIngestError::ScanIo {
            operation,
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::io::Write as _;

    use tracedecay_privacy::PrivacySanitizerError;

    use super::*;
    use crate::admission::{HostAdmissionOutcome, HostAdmissionStatus};
    use crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority;
    use crate::runtime::snapshot_observation::host_admission_error;
    use crate::runtime::source::spin_until_jsonl_change_settled;

    fn refusal(status: HostAdmissionStatus, retryable: bool) -> TranscriptIngestError {
        host_admission_error(
            "codex",
            HostAdmissionOutcome {
                status,
                retryable,
                reason_code: Some("observation_cursor_advance_ledger_disagreement"),
                recovery: None,
                cause: None,
            },
        )
    }

    fn permanent() -> TranscriptIngestError {
        refusal(HostAdmissionStatus::Degraded, false)
    }

    fn registered_hub(home: &Path) -> CodexDiscoveryHub {
        install_test_shared_jsonl_preparation_authority();
        let hub = CodexDiscoveryHub::default();
        hub.register("consumer", Some(home));
        hub
    }

    fn sweep(hub: &CodexDiscoveryHub) -> SourceSweep {
        SourceSweep::new("codex", Some((hub, "consumer")))
    }

    fn append(path: &Path, bytes: &[u8]) {
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    fn reports(sweep: SourceSweep) -> Vec<TranscriptCatchUpFailure> {
        let mut outcome = ProviderRunOutcome::skipped();
        sweep.report_into(&mut outcome);
        outcome.failures
    }

    #[tokio::test]
    async fn a_refused_file_is_skipped_until_it_changes() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("rollout.jsonl");
        std::fs::write(&path, "{}\n").unwrap();
        if !spin_until_jsonl_change_settled(&path) {
            return;
        }
        let hub = registered_hub(home.path());

        let mut first = sweep(&hub);
        let claim = first.observe_file(&path).await.unwrap().unwrap();
        assert!(first.refused(claim, permanent()).is_ok());
        assert!(
            first.observe_file(&path).await.unwrap().is_none(),
            "a refused file is not read again in the same pass"
        );
        let reported = reports(first);
        assert_eq!(reported.len(), 1);
        assert_eq!(
            reported[0].reason_code,
            "observation_cursor_advance_ledger_disagreement"
        );

        assert!(
            sweep(&hub).observe_file(&path).await.unwrap().is_none(),
            "the unchanged file stays quarantined"
        );

        append(&path, b"{}\n");
        assert!(spin_until_jsonl_change_settled(&path));
        assert!(
            sweep(&hub).observe_file(&path).await.unwrap().is_some(),
            "a changed file leaves quarantine"
        );
    }

    #[tokio::test]
    async fn a_refused_file_set_is_released_by_any_member_change() {
        let home = tempfile::tempdir().unwrap();
        let task = home.path().join("task");
        std::fs::create_dir_all(&task).unwrap();
        let api = task.join("api_conversation_history.json");
        let ui = task.join("ui_messages.json");
        let metadata = task.join("task_metadata.json");
        std::fs::write(&api, "[]").unwrap();
        std::fs::write(&ui, "[]").unwrap();
        if !spin_until_jsonl_change_settled(&api) {
            return;
        }
        assert!(spin_until_jsonl_change_settled(&ui));
        let members = vec![api.clone(), ui.clone(), metadata.clone()];
        let hub = registered_hub(home.path());
        let quarantine = |claim: Option<SourceClaim>| {
            let mut pass = sweep(&hub);
            pass.refused(claim.expect("the changed task is read"), permanent())
                .unwrap();
        };
        let observe = || async {
            sweep(&hub)
                .observe_files(&task, members.clone())
                .await
                .unwrap()
        };

        let mut first = sweep(&hub);
        let claim = first
            .observe_files(&task, members.clone())
            .await
            .unwrap()
            .unwrap();
        first.refused(claim, permanent()).unwrap();
        assert!(
            first
                .observe_files(&task, members.clone())
                .await
                .unwrap()
                .is_none(),
            "a refused task is not read again in the same pass"
        );
        assert!(
            observe().await.is_none(),
            "the unchanged task stays quarantined"
        );

        std::fs::write(&metadata, "{}").unwrap();
        assert!(spin_until_jsonl_change_settled(&metadata));
        quarantine(observe().await);
        assert!(observe().await.is_none());

        append(&ui, b" ");
        assert!(spin_until_jsonl_change_settled(&ui));
        quarantine(observe().await);
        assert!(observe().await.is_none());

        std::fs::remove_file(&metadata).unwrap();
        assert!(
            observe().await.is_some(),
            "a member disappearing releases the task"
        );
    }

    #[tokio::test]
    async fn a_refused_stream_is_released_when_its_first_uncovered_record_changes() {
        let home = tempfile::tempdir().unwrap();
        let db = home.path().join("state.db");
        let hub = registered_hub(home.path());

        let mut first = sweep(&hub);
        let claim = first
            .observe_stream(&db, "session", &[b"refused record"])
            .unwrap();
        first.refused(claim, permanent()).unwrap();
        assert!(
            first
                .observe_stream(&db, "session", &[b"a later record"])
                .is_none(),
            "the stream's later records are skipped in the same pass"
        );
        assert!(
            first
                .observe_stream(&db, "sibling", &[b"refused record"])
                .is_some(),
            "a sibling stream in the same container is still read"
        );
        assert_eq!(reports(first).len(), 1);

        let mut second = sweep(&hub);
        assert!(
            second
                .observe_stream(&db, "session", &[b"refused record"])
                .is_none(),
            "the unchanged first record keeps the stream quarantined"
        );
        assert!(
            second
                .observe_stream(&db, "session", &[b"a later record"])
                .is_none(),
            "a skipped stream stays blocked for the rest of the pass"
        );

        assert!(
            sweep(&hub)
                .observe_stream(&db, "session", &[b"rewritten record"])
                .is_some(),
            "a different first record releases the stream"
        );
    }

    #[tokio::test]
    async fn only_a_permanent_record_refusal_is_absorbed() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("rollout.jsonl");
        std::fs::write(&path, "{}\n").unwrap();
        spin_until_jsonl_change_settled(&path);
        let hub = registered_hub(home.path());

        for (error, reason_code) in [
            (
                refusal(HostAdmissionStatus::Backpressured, true),
                "observation_cursor_advance_ledger_disagreement",
            ),
            (
                TranscriptIngestError::Privacy(PrivacySanitizerError::InvalidPolicy),
                "transcript_privacy_rejected",
            ),
        ] {
            let mut pass = sweep(&hub);
            let claim = pass.observe_file(&path).await.unwrap().unwrap();
            let returned = pass.refused(claim, error).unwrap_err();
            assert_eq!(
                classify_transcript_ingest_failure("codex", "observation", &returned).reason_code,
                reason_code,
                "the caller's own error comes back"
            );
            assert!(
                pass.observe_file(&path).await.unwrap().is_some(),
                "the source is not blocked for the pass"
            );
            assert!(reports(pass).is_empty(), "nothing is reported");
            assert!(
                sweep(&hub).observe_file(&path).await.unwrap().is_some(),
                "nothing is quarantined"
            );
        }
    }

    #[tokio::test]
    async fn a_sweep_without_a_ledger_hands_a_permanent_refusal_back() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("rollout.jsonl");
        std::fs::write(&path, "{}\n").unwrap();
        let mut detached = SourceSweep::new("codex", None);

        let claim = detached.observe_file(&path).await.unwrap().unwrap();
        assert!(detached.refused(claim, permanent()).is_err());
        assert!(
            detached.observe_file(&path).await.unwrap().is_some(),
            "the source is not blocked"
        );
        assert!(reports(detached).is_empty(), "nothing is absorbed");
    }

    #[test]
    fn quarantine_reports_precede_the_failure_that_ended_the_run() {
        let home = tempfile::tempdir().unwrap();
        let hub = registered_hub(home.path());
        let mut pass = sweep(&hub);
        let claim = pass.observe_stream(Path::new("state.db"), "session", &[b"record"]);
        pass.refused(claim.unwrap(), permanent()).unwrap();
        let stopping = classify_transcript_ingest_failure(
            "codex",
            "observation",
            &refusal(HostAdmissionStatus::Backpressured, true),
        );
        let mut outcome = ProviderRunOutcome::failed(stopping, 0);

        pass.report_into(&mut outcome);

        assert_eq!(outcome.failures.len(), 2);
        assert!(!outcome.failures[0].retryable);
        assert_eq!(outcome.failures[1], stopping);
        assert!(outcome.retryable(), "the stopping failure stays last");
    }
}
