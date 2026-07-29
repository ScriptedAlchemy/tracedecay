"""Small deterministic catalog used by the runtime fixture."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class CatalogItem:
    sku: str
    label: str
    quantity: int


def fixture_catalog() -> tuple[CatalogItem, ...]:
    return (
        CatalogItem(sku="trace-001", label="Trace index", quantity=3),
        CatalogItem(sku="graph-002", label="Graph edge", quantity=5),
    )


def total_quantity(items: tuple[CatalogItem, ...]) -> int:
    return sum(item.quantity for item in items)
