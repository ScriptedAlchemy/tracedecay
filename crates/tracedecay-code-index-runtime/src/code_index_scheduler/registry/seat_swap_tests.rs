/// Clone-fingerprint backfill is still unfinished after exact and lexical
/// owners are ready. That successor is not `published_text_owner_unfinished`.
#[test]
fn unfinished_clone_fingerprint_successor_is_not_text_projection_unfinished() {
    assert!(
        !super::text_projection_unfinished_withholds_seat(true),
        "ready exact and lexical owners must still seat while the clone successor runs"
    );
    assert!(
        super::text_projection_unfinished_withholds_seat(false),
        "missing exact or lexical owners still withhold the seat"
    );
}
