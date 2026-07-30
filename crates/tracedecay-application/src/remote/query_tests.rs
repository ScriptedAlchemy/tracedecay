use super::query::RemoteQueryPageBoundsV1;

#[test]
fn remote_query_page_bounds_reject_zero_page_size() {
    assert!(RemoteQueryPageBoundsV1::new(0, None).is_err());
}
