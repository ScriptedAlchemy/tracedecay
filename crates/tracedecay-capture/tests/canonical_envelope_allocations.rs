// Its own test binary, not a capture_suite module: the counting
// #[global_allocator] below is binary-global.
//! Finishing a canonical envelope must cost one structural conversion, not an
//! encode-to-bytes plus a decode of those bytes. A counting allocator measures
//! the finishing step alone: the native record is decoded and the envelope is
//! built before the counter is armed, and the pre-conversion path is run on the
//! same envelope for comparison.
//!
//! This binary holds exactly one test because the counter is process-global.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::{Value, json};
use tracedecay_capture::{
    MAX_OBSERVATION_RECORD_BYTES, ObservationRecordParseErrorV1,
    normalize_prepared_observation_record_v1, parse_normalized_observation_record_v1,
    prepare_observation_record_v1,
};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
    ObservationOrderingDomainV1, ObservationSourceRangeV1, ProviderId, SessionId,
};

struct CountingAllocator;

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

impl CountingAllocator {
    fn record(layout: Layout) {
        if TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record(layout);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::record(layout);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record(Layout::from_size_align(new_size, layout.align()).unwrap_or(layout));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// Allocation count and bytes for `work`, measured with the counter armed.
fn measure<T>(work: impl FnOnce() -> T) -> (T, usize, usize) {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
    let output = work();
    TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);
    (
        output,
        ALLOCATIONS.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed),
    )
}

fn message_envelope(
    content: Value,
    range: ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new("codex").unwrap(),
        "message",
        ObservationId::new("record.canonical-allocations").unwrap(),
        CanonicalObservationRelationsV1::new(
            SessionId::new("session.canonical-allocations").unwrap(),
        ),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content,
            model: None,
            timestamp: None,
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range),
    )
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

fn native_record(content: &Value) -> (Vec<u8>, ObservationSourceRangeV1) {
    let record = serde_json::to_vec(&json!({ "content": content })).unwrap();
    let range = ObservationSourceRangeV1::new(0, record.len() as u64).unwrap();
    (record, range)
}

/// A representative accepted Codex-shaped payload: a large escaped text block
/// beside a nested tool invocation and its result.
fn representative_content() -> Value {
    let escaped = "quote \" backslash \\ newline \n tab \t unicode ✓ 🚀 ".repeat(1_536);
    json!([
        { "type": "text", "text": escaped },
        { "type": "tool_use", "id": "toolu_01", "name": "edit", "input": {
            "path": "src/lib.rs",
            "edits": [{ "line": 1, "text": "fn main() {}" }, { "line": 2, "text": "" }],
            "flags": { "dry_run": false, "retries": 0, "nested": { "deeper": [[[]]] } }
        } },
        { "type": "tool_result", "tool_use_id": "toolu_01", "content": [
            { "type": "text", "text": "ok" }, { "type": "json", "value": null }
        ], "is_error": false }
    ])
}

#[test]
fn finishing_a_canonical_envelope_costs_one_structural_conversion() {
    let content = representative_content();
    let (record, range) = native_record(&content);
    let envelope = message_envelope(content.clone(), range).unwrap();
    let canonical_len = serde_json::to_vec(&envelope).unwrap().len();
    // Warm anything lazily initialised so only the finishing work is measured.
    black_box(
        parse_normalized_observation_record_v1(
            &record,
            range,
            ObservationOrderingDomainV1::FileBytes,
            |native| message_envelope(native["content"].clone(), range),
        )
        .unwrap(),
    );

    // Building the envelope from the shared native tree is common to both
    // paths; measure it once and subtract it from each.
    let prepared =
        prepare_observation_record_v1(&record, range, ObservationOrderingDomainV1::FileBytes)
            .unwrap();
    let native = serde_json::from_slice::<Value>(&record).unwrap();
    let (_, build_allocations, build_bytes) =
        measure(|| black_box(message_envelope(native["content"].clone(), range).unwrap()));

    let (token, shared_allocations, shared_bytes) = measure(|| {
        normalize_prepared_observation_record_v1(black_box(prepared), |native| {
            message_envelope(native["content"].clone(), range)
        })
        .unwrap()
    });
    let finish_allocations = shared_allocations - build_allocations;
    let finish_bytes = shared_bytes - build_bytes;

    // The pre-conversion finishing path on the same envelope: encode the
    // envelope, bound the bytes, decode them back into a `Value`.
    let (round_trip, round_trip_allocations, round_trip_bytes) = measure(|| {
        let envelope = black_box(message_envelope(native["content"].clone(), range).unwrap());
        envelope.validate().unwrap();
        let bytes = serde_json::to_vec(&envelope).unwrap();
        assert!(bytes.len() <= MAX_OBSERVATION_RECORD_BYTES);
        serde_json::from_slice::<Value>(&bytes).unwrap()
    });
    let round_trip_allocations = round_trip_allocations - build_allocations;
    let round_trip_bytes = round_trip_bytes - build_bytes;
    assert_eq!(token.value(), &round_trip);

    // The owned single-record path decodes the native record once and moves it
    // through a consuming normalizer, against the shape it had before: a
    // whole-tree copy of the fresh native record just to reach that normalizer.
    let consume = |mut native: Value| message_envelope(native["content"].take(), range);
    let (owned, owned_allocations, owned_bytes) = measure(|| {
        parse_normalized_observation_record_v1(
            black_box(&record),
            range,
            ObservationOrderingDomainV1::FileBytes,
            consume,
        )
        .unwrap()
    });
    assert_eq!(owned.value(), token.value());
    let (copied, copied_allocations, copied_bytes) = measure(|| {
        let prepared = prepare_observation_record_v1(
            black_box(&record),
            range,
            ObservationOrderingDomainV1::FileBytes,
        )
        .unwrap();
        normalize_prepared_observation_record_v1(prepared, |native| consume(native.clone()))
            .unwrap()
    });
    assert_eq!(copied.value(), owned.value());

    eprintln!(
        "canonical envelope of {canonical_len} bytes from a {}-byte record: finish now \
         {finish_allocations} allocations / {finish_bytes} bytes; encoded round trip \
         {round_trip_allocations} allocations / {round_trip_bytes} bytes; owned single-record \
         path {owned_allocations} allocations / {owned_bytes} bytes; with the whole-tree copy \
         {copied_allocations} allocations / {copied_bytes} bytes",
        record.len()
    );

    // The removed byte buffer alone is at least the canonical encoding, so the
    // finishing step must now cost at least that much less than the round trip.
    assert!(
        finish_bytes + canonical_len <= round_trip_bytes,
        "finishing allocated {finish_bytes} bytes against {round_trip_bytes} for the encoded \
         round trip of a {canonical_len}-byte envelope"
    );
    assert!(
        finish_allocations < round_trip_allocations,
        "finishing took {finish_allocations} allocations against {round_trip_allocations}"
    );
    // The whole-tree copy is at least the escaped text block, so the owned path
    // must come in at least that far under it.
    let text_len = content[0]["text"].as_str().unwrap().len();
    assert!(
        owned_bytes + text_len <= copied_bytes,
        "owned path allocated {owned_bytes} bytes against {copied_bytes} with the native copy"
    );

    // A record whose envelope is just over the canonical limit is refused
    // without a limit-sized canonical buffer: the only large allocation is the
    // native decode itself. The envelope skips the constructor's validation so
    // the finishing boundary is what refuses it.
    let oversized = Value::String("a".repeat(MAX_OBSERVATION_RECORD_BYTES - 64));
    let (record, range) = native_record(&oversized);
    assert!(record.len() <= MAX_OBSERVATION_RECORD_BYTES);
    let mut shell = serde_json::to_value(message_envelope(Value::Null, range).unwrap()).unwrap();
    let (refused, _, refused_bytes) = measure(|| {
        parse_normalized_observation_record_v1(
            black_box(&record),
            range,
            ObservationOrderingDomainV1::FileBytes,
            |mut native| {
                shell["facts"][0]["content"] = native["content"].take();
                serde_json::from_value::<CanonicalObservationEnvelopeV1>(shell.take())
                    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
            },
        )
        .err()
    });
    assert_eq!(
        refused,
        Some(ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)
    );
    eprintln!(
        "just-over-limit refusal: {refused_bytes} bytes allocated for a {}-byte record",
        record.len()
    );
    assert!(
        refused_bytes < 2 * record.len(),
        "refusing an oversized envelope allocated {refused_bytes} bytes"
    );
}
