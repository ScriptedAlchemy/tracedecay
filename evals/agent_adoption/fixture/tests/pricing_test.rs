use orders_fixture::pricing::{compute_total, LineItem};

#[test]
fn total_of_two_items() {
    let items = vec![
        LineItem {
            sku: "a".into(),
            unit_price: 100,
            quantity: 2,
        },
        LineItem {
            sku: "b".into(),
            unit_price: 50,
            quantity: 1,
        },
    ];
    assert_eq!(compute_total(&items), 250);
}
