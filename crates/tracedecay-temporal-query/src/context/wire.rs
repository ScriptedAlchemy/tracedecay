use std::io::{self, Write};

use serde::ser::{SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};
use tracedecay_domain::{CompactContextBundleV1, ContextOmissionReasonV1, HydrationStateV1};

use super::super::ports::{ExecutionControl, TemporalPortError};
use super::super::resolution::summary::SummaryOmission;
use super::estimation::{TOKEN_SCAN_CHUNK_BYTES, TokenSummary};
use super::{ContextError, ContextPayload, MAX_CONTEXT_OUTPUT_BYTES, TokenPolicy};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireMeasure {
    pub bytes: u64,
    summary: TokenSummary,
}

impl WireMeasure {
    pub fn empty(policy: TokenPolicy) -> Result<Self, ContextError> {
        Ok(Self {
            bytes: 0,
            summary: TokenSummary::empty(policy)?,
        })
    }

    pub fn concatenate(&self, right: &Self) -> Result<Self, ContextError> {
        Ok(Self {
            bytes: self
                .bytes
                .checked_add(right.bytes)
                .ok_or(ContextError::BudgetExceeded { resource: "byte" })?,
            summary: self.summary.concatenate(&right.summary)?,
        })
    }

    pub fn tokens(&self) -> u64 {
        self.summary.tokens()
    }
}

pub struct StreamingWriter<'a> {
    measure: WireMeasure,
    output: Option<String>,
    pending: [u8; TOKEN_SCAN_CHUNK_BYTES + 3],
    pending_len: usize,
    invalid_utf8: bool,
    interrupted: Option<TemporalPortError>,
    control: &'a ExecutionControl,
    policy: TokenPolicy,
}

impl<'a> StreamingWriter<'a> {
    pub fn measuring(
        policy: TokenPolicy,
        control: &'a ExecutionControl,
    ) -> Result<Self, ContextError> {
        Ok(Self {
            measure: WireMeasure::empty(policy)?,
            output: None,
            pending: [0; TOKEN_SCAN_CHUNK_BYTES + 3],
            pending_len: 0,
            invalid_utf8: false,
            interrupted: None,
            control,
            policy,
        })
    }

    pub fn collecting(
        policy: TokenPolicy,
        exact_bytes: u64,
        control: &'a ExecutionControl,
    ) -> Result<Self, ContextError> {
        if exact_bytes > MAX_CONTEXT_OUTPUT_BYTES {
            return Err(ContextError::BudgetExceeded { resource: "byte" });
        }
        let capacity = usize::try_from(exact_bytes)
            .map_err(|_| ContextError::BudgetExceeded { resource: "byte" })?;
        let mut output = String::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| ContextError::BudgetExceeded {
                resource: "allocation",
            })?;
        let mut writer = Self::measuring(policy, control)?;
        writer.output = Some(output);
        Ok(writer)
    }

    fn process_pending(&mut self, final_chunk: bool) -> io::Result<()> {
        while self.pending_len != 0 {
            let consume = match std::str::from_utf8(&self.pending[..self.pending_len]) {
                Ok(_) => self.pending_len,
                Err(error) if error.error_len().is_none() && !final_chunk => {
                    let valid = error.valid_up_to();
                    if valid == 0 {
                        break;
                    }
                    valid
                }
                Err(_) => {
                    self.invalid_utf8 = true;
                    return Err(io::Error::other("canonical context is not UTF-8"));
                }
            };
            self.process_pending_prefix(consume)?;
            if consume == self.pending_len {
                self.pending_len = 0;
            } else {
                self.pending.copy_within(consume..self.pending_len, 0);
                self.pending_len -= consume;
                break;
            }
        }
        Ok(())
    }

    fn process_pending_prefix(&mut self, len: usize) -> io::Result<()> {
        if let Err(error) = self.control.checkpoint() {
            self.interrupted = Some(error);
            return Err(io::Error::other("compact context assembly interrupted"));
        }
        let scanned = {
            let fragment = std::str::from_utf8(&self.pending[..len])
                .map_err(|_| io::Error::other("invalid UTF-8 prefix"))?;
            TokenSummary::scan(self.policy, fragment, self.control)
        }
        .map_err(|error| {
            if let ContextError::Interrupted(interrupted) = error {
                self.interrupted = Some(interrupted);
            }
            io::Error::other("compact context token scan failed")
        })?;
        self.measure.summary = self
            .measure
            .summary
            .concatenate(&scanned)
            .map_err(|_| io::Error::other("compact context token accounting overflow"))?;
        if let Some(output) = &mut self.output {
            let fragment = std::str::from_utf8(&self.pending[..len])
                .map_err(|_| io::Error::other("invalid UTF-8 prefix"))?;
            let required = output
                .len()
                .checked_add(fragment.len())
                .ok_or_else(|| io::Error::other("compact context output overflow"))?;
            if required > output.capacity() {
                output
                    .try_reserve_exact(required - output.len())
                    .map_err(|_| io::Error::other("compact context allocation failed"))?;
            }
            output.push_str(fragment);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn output_capacity(&self) -> usize {
        self.output.as_ref().map_or(0, String::capacity)
    }

    pub fn finish(
        mut self,
        result: Result<(), serde_json::Error>,
    ) -> Result<(WireMeasure, Option<String>), ContextError> {
        let pending_result = self.process_pending(true);
        if let Some(error) = self.interrupted.clone() {
            return Err(ContextError::Interrupted(error));
        }
        if self.invalid_utf8 {
            return Err(ContextError::InvalidBundle(
                "canonical context was not UTF-8".to_string(),
            ));
        }
        result.map_err(|error| ContextError::InvalidBundle(error.to_string()))?;
        pending_result.map_err(|error| ContextError::InvalidBundle(error.to_string()))?;
        Ok((self.measure, self.output))
    }
}

impl Write for StreamingWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if let Err(error) = self.control.checkpoint() {
            self.interrupted = Some(error);
            return Err(io::Error::other("compact context assembly interrupted"));
        }
        self.measure.bytes = self
            .measure
            .bytes
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| io::Error::other("compact context byte accounting overflow"))?;
        let mut remaining = buffer;
        while !remaining.is_empty() {
            let available = self.pending.len() - self.pending_len;
            let take = available.min(remaining.len());
            self.pending[self.pending_len..self.pending_len + take]
                .copy_from_slice(&remaining[..take]);
            self.pending_len += take;
            remaining = &remaining[take..];
            if self.pending_len >= TOKEN_SCAN_CHUNK_BYTES {
                self.process_pending(false)?;
            }
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct CanonicalContextWire<'a, P: ContextPayload> {
    pub format: &'static str,
    pub estimator_version: &'a str,
    pub bundle: &'a CompactContextBundleV1,
    pub summary_omissions: &'a [SummaryOmission],
    pub payloads: CanonicalPayloads<'a, P>,
}

impl<P: ContextPayload> Serialize for CanonicalContextWire<'_, P> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut wire = serializer.serialize_struct("CanonicalContextWire", 5)?;
        wire.serialize_field("format", self.format)?;
        wire.serialize_field("estimator_version", self.estimator_version)?;
        wire.serialize_field("bundle", self.bundle)?;
        wire.serialize_field("summary_omissions", self.summary_omissions)?;
        wire.serialize_field("payloads", &self.payloads)?;
        wire.end()
    }
}

pub struct CanonicalPayloads<'a, P>(pub &'a [P]);

impl<P: ContextPayload> Serialize for CanonicalPayloads<'_, P> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for payload in self.0 {
            sequence.serialize_element(&CanonicalPayload(payload))?;
        }
        sequence.end()
    }
}

pub struct CanonicalPayload<'a, P>(pub &'a P);

impl<P: ContextPayload> Serialize for CanonicalPayload<'_, P> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut frame = serializer.serialize_struct("CanonicalPayload", 3)?;
        frame.serialize_field("anchor_id", self.0.anchor_id())?;
        if let Ok(text) = std::str::from_utf8(self.0.bytes()) {
            frame.serialize_field("encoding", "utf8")?;
            frame.serialize_field("data", text)?;
        } else {
            frame.serialize_field("encoding", "bytes")?;
            frame.serialize_field("data", self.0.bytes())?;
        }
        frame.end()
    }
}

pub const fn omission_reason(state: HydrationStateV1) -> ContextOmissionReasonV1 {
    match state {
        HydrationStateV1::Unauthorized => ContextOmissionReasonV1::Unauthorized,
        HydrationStateV1::Redacted => ContextOmissionReasonV1::Redacted,
        HydrationStateV1::Deleted => ContextOmissionReasonV1::Deleted,
        HydrationStateV1::RetentionExpired => ContextOmissionReasonV1::RetentionExpired,
        HydrationStateV1::Locked => ContextOmissionReasonV1::Locked,
        HydrationStateV1::Available
        | HydrationStateV1::RetainedButUnavailable
        | HydrationStateV1::UnverifiableLegacy => ContextOmissionReasonV1::Unavailable,
    }
}
