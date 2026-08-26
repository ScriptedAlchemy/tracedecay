use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_code_index::production::CodeIndexExecutionControlV1;

use super::{CodeIndexIgnoredDependencyRefusalV1, GitFixture, assert_refusal};

struct CancelAfterChecks {
    checks: AtomicUsize,
    cancel_on: usize,
}

impl CancelAfterChecks {
    fn new(cancel_on: usize) -> Self {
        Self {
            checks: AtomicUsize::new(0),
            cancel_on,
        }
    }
}

impl CodeIndexExecutionControlV1 for CancelAfterChecks {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::AcqRel) + 1 >= self.cancel_on
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

#[test]
fn admitted_source_read_observes_live_cancellation_between_chunks() {
    let fixture = GitFixture::new("import type { Widget } from \"pkg\";\n");
    fixture.write(
        "node_modules/pkg/index.d.ts",
        vec![b' '; 2 * super::super::ignored_dependencies::ADMITTED_SOURCE_READ_CHUNK_BYTES],
    );
    // Admission validates the roster path before the bounded reader begins.
    // Cancel on the second read checkpoint, after one full chunk was observed.
    let control = CancelAfterChecks::new(5);

    let error = super::super::ignored_dependencies::read_bounded_admitted_source(
        fixture.path(),
        "node_modules/pkg/index.d.ts",
        Some(&control),
    )
    .expect_err("live cancellation must interrupt a multi-chunk admitted-source read");

    assert_refusal(error, CodeIndexIgnoredDependencyRefusalV1::Cancelled);
    assert_eq!(control.checks.load(Ordering::Acquire), 5);
}

#[test]
fn ordinary_snapshot_read_observes_live_cancellation_between_chunks() {
    let fixture = GitFixture::new("export function ordinary() { return 1; }\n");
    fixture.write(
        "src/large.ts",
        vec![b' '; 2 * super::super::ignored_dependencies::ADMITTED_SOURCE_READ_CHUNK_BYTES],
    );
    let control = CancelAfterChecks::new(2);

    let error = super::super::ignored_dependencies::read_bounded_snapshot_source(
        &fixture.path().join("src/large.ts"),
        Some(&control),
    )
    .expect_err("live cancellation must interrupt a multi-chunk ordinary-source read");

    assert_refusal(error, CodeIndexIgnoredDependencyRefusalV1::Cancelled);
    assert_eq!(control.checks.load(Ordering::Acquire), 2);
}
