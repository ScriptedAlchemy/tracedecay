#!/usr/bin/env python3
"""No-model execution regressions for the stock-edit oracle.

Run from the checkout, outside a Bazel test process: the oracle itself invokes
Bazel to compile each captured candidate against the same trusted assertions
used by grade.py. A valid edit must execute successfully, so missing build
infrastructure cannot make this regression suite green.
"""
from __future__ import annotations

import json
from pathlib import Path

import grade

HERE = Path(__file__).resolve().parent


def main() -> int:
    fixture = HERE / "fixture"
    renamed = {path.relative_to(fixture).as_posix(): path.read_text().replace(
        "check_availability", "has_sufficient_stock"
    ) for path in (fixture / "src").rglob("*.rs")}
    equivalent = {**renamed, "src/inventory.rs": renamed["src/inventory.rs"].replace(
        "self.on_hand.get(sku).copied().unwrap_or(0) >= qty",
        "self.on_hand.get(sku).map_or(qty == 0, |stock| *stock >= qty)",
    )}
    strict = {**renamed, "src/inventory.rs": renamed["src/inventory.rs"].replace(
        ">= qty", "> qty",
    )}
    no_op = {**renamed, "src/orders.rs": renamed["src/orders.rs"].split("pub fn place_order")[0]
             + "pub fn place_order(_wh: &mut Warehouse, _order: &Order) -> Result<u64, String> { Ok(0) }\n"}
    destructive = {**renamed, "src/orders.rs": "// has_sufficient_stock\npub fn place_order() {}\n"}
    alias = {**renamed, "src/inventory.rs": renamed["src/inventory.rs"] + '''
impl Warehouse {
    pub fn check_availability(&self, sku: &str, qty: u32) -> bool {
        self.has_sufficient_stock(sku, qty)
    }
}
'''}
    scenario = json.loads((HERE / "scenarios/baseline_stock_check_rename.json").read_text())
    transcript = grade.load_transcript_lines([
        json.dumps({"type": "result", "result": "The rename is complete."}),
    ], "claude")
    failures = 0
    for name, sources, expected in (
        ("valid rename", renamed, "pass"),
        ("equivalent stock implementation", equivalent, "pass"),
        ("strict boundary regression", strict, "fail"),
        ("no-op order placement", no_op, "fail"),
        ("deleted order implementation", destructive, "unverified"),
        ("legacy method retained", alias, "unverified"),
    ):
        result = grade.grade_outcome(scenario, transcript, sources)
        passed = result["status"] == expected
        print(f"{'ok' if passed else 'FAIL'}: {name}: {result['status']}", flush=True)
        if not passed:
            failures += 1
            print(json.dumps(result, indent=2))
    return int(failures != 0)


if __name__ == "__main__":
    raise SystemExit(main())
