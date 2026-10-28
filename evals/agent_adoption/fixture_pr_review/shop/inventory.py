"""Stock reservations used by order placement."""


def reserve_stock(warehouse: dict[str, int], sku: str, quantity: int) -> None:
    """Reserve stock or raise when the requested quantity is unavailable."""
    if warehouse.get(sku, 0) < quantity:
        raise ValueError(f"insufficient stock for {sku}")
    warehouse[sku] -= quantity
