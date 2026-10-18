use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, CanonicalObservationFactV1, ObservationOrderingDomainV1,
};

use crate::runtime::source::{RawJsonlFrame, RawJsonlFrameReader};
use tracedecay_privacy::{MAX_OBSERVATION_RECORD_BYTES, parse_observation_record_v1};

use super::CWD_PROBE_LINES;

pub fn transcript_cwd(path: &Path) -> Option<PathBuf> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut frames = RawJsonlFrameReader::new(reader, MAX_OBSERVATION_RECORD_BYTES);
    let mut offset = 0_u64;
    for _ in 0..CWD_PROBE_LINES {
        let byte_len = match frames.next_frame().ok()? {
            RawJsonlFrame::Eof
            | RawJsonlFrame::Partial { .. }
            | RawJsonlFrame::Oversized { .. }
            | RawJsonlFrame::BudgetExhausted { .. } => return None,
            RawJsonlFrame::Complete { byte_len } => byte_len,
        };
        let end_offset = offset.checked_add(byte_len)?;
        let record = frames.record();
        if record.iter().all(u8::is_ascii_whitespace) {
            offset = end_offset;
            continue;
        }
        let range = tracedecay_domain::ObservationSourceRangeV1::new(offset, end_offset).ok()?;
        if let Ok(parsed) =
            parse_observation_record_v1(record, range, ObservationOrderingDomainV1::FileBytes)
            && let Some(cwd) = parsed.value().get("cwd").and_then(Value::as_str)
            && !cwd.is_empty()
        {
            return Some(PathBuf::from(cwd));
        }
        offset = end_offset;
    }
    None
}

/// Read a record's `cwd`, falling back to the canonical envelope's session
/// location fact.
pub(super) fn record_cwd(record: &Value) -> Option<PathBuf> {
    if let Some(cwd) = record
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
    {
        return Some(PathBuf::from(cwd));
    }
    let Ok(envelope) = serde_json::from_value::<CanonicalObservationEnvelopeV1>(record.clone())
    else {
        return None;
    };
    envelope.facts().iter().find_map(|fact| match fact {
        CanonicalObservationFactV1::Session {
            location_path: Some(path),
            ..
        } if !path.is_empty() => Some(PathBuf::from(path)),
        _ => None,
    })
}
