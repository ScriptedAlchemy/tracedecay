use tracedecay_capture::{civil_from_days, parse_cursor_human_timestamp, parse_rfc3339_timestamp};

#[test]
fn exposes_timestamp_parsing_kernel() {
    assert_eq!(
        parse_rfc3339_timestamp("1970-01-01T02:00:00+02:00"),
        Some(0)
    );
    assert_eq!(
        parse_cursor_human_timestamp("Thursday, Jan 1, 1970, 12:00 AM (UTC)"),
        Some(0)
    );
    assert_eq!(civil_from_days(0), (1970, 1, 1));
}
