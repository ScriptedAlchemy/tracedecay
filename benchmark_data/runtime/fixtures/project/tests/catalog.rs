use runtime_fixture::fixture_catalog_total;

#[test]
fn fixture_catalog_has_stable_total() {
    assert_eq!(fixture_catalog_total(&[3, 5]), 8);
    assert_eq!(fixture_catalog_total(&[]), 0);
}
