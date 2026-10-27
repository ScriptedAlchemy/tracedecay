#!/usr/bin/env python3
"""Build the immutable two-revision source-review fixture used by live evals.

The fixture has one silent behavioral regression at an unchanged caller and
one mechanically moved module whose public import still works. Its baseline
and head are ordinary Git commits, so native and product reads see the same
comparison. No answer key is placed inside the agent's checkout.
"""
from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys


HEAD_INVENTORY = '''"""Stock reservations used by order placement."""


def reserve_stock(warehouse: dict[str, int], sku: str, quantity: int) -> bool:
    """Return whether the requested stock was reserved."""
    if warehouse.get(sku, 0) < quantity:
        return False
    warehouse[sku] -= quantity
    return True
'''


def git(root: Path, *args: str) -> str:
    environment = dict(os.environ)
    environment.update({
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_AUTHOR_DATE": "2026-06-01T12:00:00+00:00",
        "GIT_COMMITTER_DATE": "2026-06-01T12:00:00+00:00",
    })
    return subprocess.run(
        ["git", "-C", str(root), "-c", "user.name=eval", "-c",
         "user.email=eval@tracedecay", *args],
        check=True, capture_output=True, text=True, env=environment,
    ).stdout.strip()


def build(destination: Path) -> dict[str, str]:
    shutil.copytree(Path(__file__).with_name("fixture_pr_review"), destination,
                    ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    git(destination, "init", "-q", "--initial-branch=main")
    git(destination, "add", ".")
    git(destination, "commit", "-qm", "feat(shop): add stock reservation and receipt helpers")
    base = git(destination, "rev-parse", "HEAD")
    git(destination, "checkout", "-qb", "feature/stock-result")
    (destination / "shop/inventory.py").write_text(HEAD_INVENTORY, encoding="utf-8")
    (destination / "shop/receipts.py").rename(destination / "shop/receipt_format.py")
    (destination / "shop/__init__.py").write_text(
        '"""Public order and receipt helpers."""\n\n'
        'from .orders import place_order\nfrom .receipt_format import render_receipt\n',
        encoding="utf-8",
    )
    git(destination, "add", ".")
    git(destination, "commit", "-qm", "refactor(shop): return reservation result and move receipt formatter")
    return {"base": base, "head": git(destination, "rev-parse", "HEAD")}


if __name__ == "__main__":
    build(Path(sys.argv[1]))
