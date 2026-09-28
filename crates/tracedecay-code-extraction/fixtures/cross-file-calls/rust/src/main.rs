use fixture::math::scale;
use fixture::store::Store;
use fixture::{compat, legacy, report, shapes};

fn main() {
    let mut store = Store::default();
    store.add("Key", 1);
    store.get("key");
    println!("{}", report::summary(&[1, 2, 3]));
    println!("{}", compat::upgrade(" Text "));
    println!("{} {}", shapes::area(3, 4), shapes::perimeter(3, 4));
    println!("{} {}", legacy::old_format("x"), scale(2, 3));
}
