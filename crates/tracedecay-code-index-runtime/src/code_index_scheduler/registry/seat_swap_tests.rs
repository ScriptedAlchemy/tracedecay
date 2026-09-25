/// Only missing query owners withhold the seat.
#[test]
fn only_missing_query_owners_withhold_the_seat() {
    assert!(
        !super::text_projection_unfinished_withholds_seat(true),
        "ready query owners must seat"
    );
    assert!(
        super::text_projection_unfinished_withholds_seat(false),
        "missing exact or lexical owners still withhold the seat"
    );
}
