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


class CatalogBase:
    """Base fixture for the inheritance graph read."""

    def label_prefix(self) -> str:
        return "catalog"


class CatalogChild(CatalogBase):
    """Concrete fixture subclass with one inherited method."""

    pass


def fixture_countdown(value: int) -> int:
    """Return the triangular number through a real recursive call edge."""
    if value <= 0:
        return 0
    return value + fixture_countdown(value - 1)
