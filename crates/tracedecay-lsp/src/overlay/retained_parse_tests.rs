use tracedecay_code_extraction::incremental::{ParseReport, ParseReuse};

use crate::diagnostics::{LspPosition, LspRange};

use super::{
    AdmittedRoot, OverlayChange, OverlayParseState, OverlayParseUnavailable, OverlayStore,
};

fn admitted_root() -> AdmittedRoot {
    AdmittedRoot::new("file:///root")
}

fn range(start: u32, end: u32) -> LspRange {
    LspRange {
        start: LspPosition {
            line: 0,
            character: start,
        },
        end: LspPosition {
            line: 0,
            character: end,
        },
    }
}

#[test]
fn ordered_utf16_changes_reuse_one_retained_tree_with_exact_input_edits() {
    let mut overlays = OverlayStore::default();
    let opened = overlays
        .open(&admitted_root(), "file:///root/a.rs", "rust", 1, "a🦀b")
        .expect("open overlay");
    assert!(matches!(
        opened.parse_state,
        OverlayParseState::Ready(ParseReport {
            reuse: ParseReuse::Initial,
            ..
        })
    ));

    let changed = overlays
        .change(
            "file:///root/a.rs",
            2,
            &[
                OverlayChange {
                    range: Some(range(1, 3)),
                    range_length: Some(2),
                    text: "cat".into(),
                },
                OverlayChange {
                    range: Some(range(1, 4)),
                    range_length: Some(3),
                    text: "dog".into(),
                },
            ],
        )
        .expect("ordered UTF-16 changes");

    assert_eq!(changed.text, "adogb");
    let OverlayParseState::Ready(report) = changed.parse_state else {
        panic!("expected retained incremental parse");
    };
    assert_eq!(report.reuse, ParseReuse::Incremental);
    assert_eq!(report.metrics.input_edit_count, 2);
    assert!(report.metrics.reused_prior_tree);
    assert!(
        report
            .changed_ranges
            .iter()
            .all(|range| range.end_byte <= changed.text.len())
    );
}

#[test]
fn unsupported_language_preserves_text_with_typed_parse_unavailable_state() {
    let mut overlays = OverlayStore::default();
    let opened = overlays
        .open(
            &admitted_root(),
            "file:///root/a.txt",
            "plaintext",
            1,
            "original",
        )
        .expect("open unsupported overlay");
    assert_eq!(
        opened.parse_state,
        OverlayParseState::Unavailable(OverlayParseUnavailable::UnsupportedLanguage)
    );

    let changed = overlays
        .change(
            "file:///root/a.txt",
            2,
            &[OverlayChange {
                range: None,
                range_length: None,
                text: "changed".into(),
            }],
        )
        .expect("text remains usable without a parse tree");
    assert_eq!(changed.text, "changed");
    assert_eq!(
        changed.parse_state,
        OverlayParseState::Unavailable(OverlayParseUnavailable::UnsupportedLanguage)
    );
}

#[test]
fn replacement_and_reopen_each_advance_the_exact_document_generation() {
    let mut overlays = OverlayStore::default();
    let opened = overlays
        .open(
            &admitted_root(),
            "file:///root/a.rs",
            "rust",
            1,
            "fn before() {}",
        )
        .expect("open overlay");
    let replaced = overlays
        .change(
            "file:///root/a.rs",
            2,
            &[OverlayChange {
                range: None,
                range_length: None,
                text: "fn after() {}".into(),
            }],
        )
        .expect("replace overlay");
    assert_eq!(replaced.document_generation, opened.document_generation + 1);
    let OverlayParseState::Ready(replacement_report) = replaced.parse_state else {
        panic!("expected replacement parse");
    };
    assert_eq!(replacement_report.reuse, ParseReuse::Reset);
    assert!(!replacement_report.metrics.reused_prior_tree);

    overlays.close("file:///root/a.rs").expect("close overlay");
    let reopened = overlays
        .open(
            &admitted_root(),
            "file:///root/a.rs",
            "rust",
            1,
            "fn reopened() {}",
        )
        .expect("reopen overlay");
    assert!(reopened.document_generation > replaced.document_generation);
    assert!(matches!(
        reopened.parse_state,
        OverlayParseState::Ready(ParseReport {
            reuse: ParseReuse::Initial,
            ..
        })
    ));
}
