//! Per-consumer memory of transcript sources a discovery consumer finished or
//! quarantined, each tied to the witness observed before reading.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tracedecay_runtime_core::resident_memory::ProcessSharedMemoryReservationV1;

use crate::runtime::hosts::codex::{candidate_charge, change_stamp, codex_corpus_identity};
use crate::runtime::jsonl_observation_admission::reserve_shared_jsonl_bytes;
use crate::runtime::source::{
    TranscriptIngestError, TranscriptIngestResult, run_blocking_transcript_section,
};

/// One discovery consumer's memory of sources it finished or quarantined.
///
/// Held by `CodexDiscoveryHub` per registered consumer, so records and their
/// memory reservations are released when the consumer deregisters. Records
/// are best-effort: without memory capacity a source is simply read again.
#[derive(Clone, Default)]
pub(crate) struct SourceLedger {
    inner: Arc<Mutex<HashMap<SourceKey, Disposition>>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SourceKey {
    origin: PathBuf,
}

/// A source's identity, taken only once its change time is settled so that an
/// equal later witness proves the bytes unchanged.
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
        }
    }
}

impl SourceLedger {
    /// Whether the source was finished or quarantined at exactly `witness`.
    pub(crate) fn holds(&self, key: &SourceKey, witness: Witness) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .is_some_and(|recorded| recorded.witness == witness)
    }

    /// Whether the source's last record is a finish. Used while its change
    /// time is still inside the coarse timestamp quantum and cannot mint a
    /// fresh witness, but an earlier pass already proved the bytes unchanged.
    pub(crate) fn recorded_converged(&self, key: &SourceKey) -> bool {
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
        let charge = candidate_charge(
            &key.origin,
            u64::try_from(std::mem::size_of::<(SourceKey, Disposition)>()).unwrap_or(u64::MAX),
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
    /// `None` while the file's change time is inside the timestamp quantum,
    /// or where no stat field witnesses a rewrite: the pass then reads it as
    /// before.
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
}

/// A source this pass decided to read. Dropping it records nothing.
///
/// Every host's history pass for a scope runs under that scope's discovery
/// consumer, so one per-consumer record proves a source unchanged for all of
/// them.
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

    /// `None` when the ledger already finished or quarantined this exact
    /// file, or the file is gone, so the pass skips it without opening it.
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

    /// [`Self::observe_file`] for a caller already on a blocking thread.
    pub(crate) fn observe_file_blocking(
        ledger: Option<&SourceLedger>,
        path: &Path,
    ) -> TranscriptIngestResult<Option<Self>> {
        let key = SourceKey::file(path);
        if ledger.is_none() {
            return Ok(Some(Self { key, settled: None }));
        }
        // A delivered pass is replayed until a catch-up acknowledges it, and a
        // read failure withholds the acknowledgement. A file moved away since
        // delivery (Codex archives a session by moving its rollout) has
        // nothing left to read; failing on it would replay the same path
        // forever. A later walk delivers the file again if it returns.
        let Some(metadata) = stat_if_present(path, "stat transcript for convergence")? else {
            return Ok(None);
        };
        let witness = Witness::settled_file(path, &metadata)?;
        Ok(Self::unless_recorded(ledger, key, witness))
    }

    /// Quarantines the source when the failure is a permanent refusal of its
    /// own bytes, so it is reported once and skipped until its witness
    /// changes. A failure of the authority (storage, a missing sanitizer, a
    /// corrupt projection store) can recover without the source changing, so
    /// the next pass reads it again.
    pub(crate) fn record_source_failure(
        self,
        error: &TranscriptIngestError,
    ) -> TranscriptIngestResult<()> {
        if crate::runtime::classify_transcript_ingest_disposition(error).quarantines_source()
            && let Some((ledger, witness)) = self.settled
        {
            ledger.record(&self.key, witness, true)?;
        }
        Ok(())
    }

    /// Records convergence once admission covered every byte the witness
    /// measured; a deferred or shorter admission reads the source again.
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

    /// Records that the pass read this source and found nothing the
    /// consumer's scope will ever admit from it unchanged (it is outside the
    /// scope, or every source it names already converged), so later passes
    /// do not re-read it to decide again.
    pub(crate) fn finished(self) -> TranscriptIngestResult<()> {
        match self.settled {
            Some((ledger, witness)) => ledger.record(&self.key, witness, false),
            None => Ok(()),
        }
    }

    /// Whether admission consumed the whole settled source and left no tail.
    /// An unsettled change time cannot prove that, so a later pass reads the
    /// source again.
    pub(crate) fn admission_settles(&self, source_deferred: bool, covered_through: u64) -> bool {
        self.settled
            .as_ref()
            .is_some_and(|(_, witness)| !source_deferred && covered_through == witness.len)
    }

    /// Owned copy of the settled witness, so a caller can finish the source
    /// after asynchronous admission without observing it again.
    pub(crate) fn settled(&self) -> Option<SettledClaim> {
        self.settled.as_ref().map(|(ledger, witness)| SettledClaim {
            ledger: ledger.clone(),
            key: self.key.clone(),
            witness: *witness,
        })
    }
}

/// A source whose change time was already settled when this pass observed it.
///
/// Held across admission so the parent file can be finished once every
/// source it names is covered, without a later pass opening it to decide.
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
