"""Nightly stock report for the demo storefront fixture."""

from __future__ import annotations


def low_stock_skus(available: dict[str, int], threshold: int) -> list[str]:
    """SKUs whose available count fell to or below the reorder threshold."""
    return sorted(sku for sku, count in available.items() if count <= threshold)


def render_stock_report(available: dict[str, int], threshold: int) -> str:
    low = low_stock_skus(available, threshold)
    if not low:
        return "all SKUs above the reorder threshold"
    return "reorder needed: " + ", ".join(low)
