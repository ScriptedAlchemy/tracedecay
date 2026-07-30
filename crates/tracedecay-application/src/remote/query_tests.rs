use super::query::{
    MAX_REMOTE_QUERY_CURSOR_BYTES_V1, RemoteQueryCompleteValueV1, RemoteQueryPageBoundsV1,
};

#[test]
fn remote_query_page_bounds_reject_zero_page_size() {
    assert!(RemoteQueryPageBoundsV1::new(0, None).is_err());
}

#[test]
fn remote_complete_value_is_wire_distinct_from_null() {
    let value = RemoteQueryCompleteValueV1 {
        complete_value_present: true,
    };

    let json = serde_json::to_string(&value).expect("serialize complete value");
    assert_eq!(json, r#"{"complete_value_present":true}"#);
    let round_trip: RemoteQueryCompleteValueV1 =
        serde_json::from_str(&json).expect("deserialize complete value");
    round_trip.validate().expect("validate complete value");
}

#[test]
fn remote_query_page_bounds_enforce_page_and_cursor_limits() {
    for page_size in [0, 101] {
        assert!(RemoteQueryPageBoundsV1::new(page_size, None).is_err());
    }
    for page_size in [1, 100] {
        assert!(RemoteQueryPageBoundsV1::new(page_size, None).is_ok());
    }
    assert!(
        RemoteQueryPageBoundsV1::new(1, Some("x".repeat(MAX_REMOTE_QUERY_CURSOR_BYTES_V1))).is_ok()
    );
    assert!(
        RemoteQueryPageBoundsV1::new(1, Some("x".repeat(MAX_REMOTE_QUERY_CURSOR_BYTES_V1 + 1)))
            .is_err()
    );
}
