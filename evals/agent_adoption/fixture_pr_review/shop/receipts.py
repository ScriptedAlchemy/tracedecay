"""Format order totals for display."""


def render_receipt(total: int) -> str:
    return f"Total: {total // 100}.{total % 100:02d}"
