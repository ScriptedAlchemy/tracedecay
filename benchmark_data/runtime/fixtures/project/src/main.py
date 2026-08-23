"""Cross-file entry point for the deterministic runtime fixture."""

from __future__ import annotations

from catalog import fixture_catalog, total_quantity


def render_summary() -> str:
    items = fixture_catalog()
    labels = ", ".join(item.label for item in items)
    return f"{labels}: {total_quantity(items)}"


if __name__ == "__main__":
    print(render_summary())
