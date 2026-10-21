use orders_fixture::inventory::{Warehouse, reserve_stock};
use orders_fixture::orders::{Order, place_order};
use orders_fixture::pricing::LineItem;

// An inherent legacy method shadows this zero-argument trait method and makes
// the call below fail to compile. This checks the removed public API without
// accepting or rejecting an implementation based on source spelling.
trait RemovedStockCheck {
    fn check_availability(&self) {}
}

impl RemovedStockCheck for Warehouse {}

fn warehouse() -> Warehouse {
    let mut warehouse = Warehouse::new();
    warehouse.on_hand.insert("a".into(), 4);
    warehouse.on_hand.insert("b".into(), 2);
    warehouse
}

fn order(items: &[(&str, u64, u32)]) -> Order {
    Order {
        id: 17,
        customer: "buyer".into(),
        items: items
            .iter()
            .map(|(sku, unit_price, quantity)| LineItem {
                sku: (*sku).into(),
                unit_price: *unit_price,
                quantity: *quantity,
            })
            .collect(),
    }
}

// A fixed main avoids libtest filters turning the oracle into a zero-test pass.
// This file is trusted grader input, outside the agent's editable fixture.
fn main() {
    let mut wh = warehouse();
    wh.check_availability();
    let before = wh.on_hand.clone();
    for (sku, qty, expected) in [
        ("a", 0, true),
        ("a", 3, true),
        ("a", 4, true),
        ("a", 5, false),
        ("missing", 0, true),
        ("missing", 1, false),
    ] {
        assert_eq!(wh.has_sufficient_stock(sku, qty), expected);
    }
    assert_eq!(wh.on_hand, before, "checking stock must not reserve it");
    assert_eq!(
        reserve_stock(&mut wh, "a", 5),
        Err("insufficient stock for a".into())
    );
    assert_eq!(
        wh.on_hand, before,
        "failed reservation must not mutate stock"
    );
    assert_eq!(reserve_stock(&mut wh, "a", 4), Ok(()));
    assert_eq!(wh.on_hand.get("a"), Some(&0));
    assert!(!wh.has_sufficient_stock("a", 1));
    assert_eq!(reserve_stock(&mut wh, "missing", 0), Ok(()));
    assert!(!wh.on_hand.contains_key("missing"));

    let mut wh = warehouse();
    let purchase = order(&[("a", 125, 4), ("b", 30, 1)]);
    assert_eq!(place_order(&mut wh, &purchase), Ok(530));
    assert_eq!(wh.on_hand.get("a"), Some(&0));
    assert_eq!(wh.on_hand.get("b"), Some(&1));
    let before = wh.on_hand.clone();
    assert_eq!(place_order(&mut wh, &order(&[])), Ok(0));
    assert_eq!(wh.on_hand, before);
    assert_eq!(
        place_order(&mut wh, &order(&[("missing", 50, 1)])),
        Err("insufficient stock for missing".into())
    );
    assert_eq!(wh.on_hand, before);
    assert_eq!(
        place_order(&mut wh, &order(&[("b", 30, 2)])),
        Err("insufficient stock for b".into())
    );
    assert_eq!(wh.on_hand, before);
}
