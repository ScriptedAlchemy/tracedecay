"""Place orders after reserving their stock."""

from .inventory import reserve_stock


def place_order(warehouse: dict[str, int], sku: str, quantity: int, unit_price: int) -> int:
    reserve_stock(warehouse, sku, quantity)
    return quantity * unit_price
