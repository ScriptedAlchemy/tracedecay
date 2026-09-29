//! Pooled canonical serialization scratch is counted while threads hold it
//! and gone once each holder releases it.
//!
//! The count is process-wide, so this is its own binary with a single test:
//! no other test may serialize while it measures.

use std::collections::BTreeMap;

use serde::Serialize;
use tracedecay_domain::research::{
    canonical_sha256, pooled_canonical_scratch_bytes, release_thread_canonical_scratch,
};

const DIGEST_SINK_BYTES: u64 = 64 * 1024;

#[derive(Serialize)]
struct Row {
    name: String,
    fields: BTreeMap<String, u64>,
}

fn digest_a_row() {
    let row = Row {
        name: "row".to_owned(),
        fields: (0..16)
            .map(|index| (format!("field-{index}"), index))
            .collect(),
    };
    canonical_sha256(&row).expect("canonical digest");
}

#[test]
fn each_thread_releases_only_its_own_pooled_scratch() {
    assert_eq!(pooled_canonical_scratch_bytes(), 0);

    digest_a_row();
    let one_thread = pooled_canonical_scratch_bytes();
    assert!(
        one_thread > DIGEST_SINK_BYTES,
        "a digesting thread keeps its digest sink and object buffers: {one_thread}"
    );

    let (both, after_other_released) = std::thread::spawn(|| {
        digest_a_row();
        let both = pooled_canonical_scratch_bytes();
        release_thread_canonical_scratch();
        (both, pooled_canonical_scratch_bytes())
    })
    .join()
    .expect("second digesting thread");
    assert_eq!(both, 2 * one_thread);
    assert_eq!(after_other_released, one_thread);

    release_thread_canonical_scratch();
    assert_eq!(pooled_canonical_scratch_bytes(), 0);

    digest_a_row();
    assert_eq!(pooled_canonical_scratch_bytes(), one_thread);
}
