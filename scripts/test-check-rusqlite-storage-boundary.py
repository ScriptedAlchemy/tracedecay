#!/usr/bin/env python3
"""Behavioral tests for the production rusqlite ownership check."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path

SCRIPT_PATH = Path(__file__).resolve().with_name(
    "check-rusqlite-storage-boundary.py"
)


def load_script():
    spec = importlib.util.spec_from_file_location(
        "check_rusqlite_storage_boundary", SCRIPT_PATH
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def dependency(
    name: str,
    *,
    kind: str | None = None,
    optional: bool = False,
    features: tuple[str, ...] = (),
    target: str | None = None,
) -> dict:
    return {
        "name": name,
        "source": None,
        "req": "*",
        "kind": kind,
        "rename": None,
        "optional": optional,
        "uses_default_features": True,
        "features": list(features),
        "target": target,
        "registry": None,
        "path": None,
    }


def package(
    name: str,
    *,
    features: dict[str, list[str]] | None = None,
    dependencies: list[dict] | None = None,
) -> dict:
    return {
        "name": name,
        "source": None,
        "features": features or {},
        "dependencies": dependencies or [],
    }


def valid_metadata() -> dict:
    packages = [
        package("tracedecay", features={"production": []}),
        package("tracedecay-cli", features={"production": []}),
    ]
    packages.extend(
        package(owner, dependencies=[dependency("rusqlite")])
        for owner in (
            "tracedecay-query",
            "tracedecay-runtime-core",
            "tracedecay-rusqlite-runtime",
            "tracedecay-sessions",
        )
    )
    return {"packages": packages}


class StorageBoundaryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.check = load_script()

    def test_allows_dev_and_isolated_test_helper_dependencies(self) -> None:
        metadata = valid_metadata()
        metadata["packages"].append(
            package(
                "fixture-consumer",
                features={"test-helpers": ["dep:rusqlite"]},
                dependencies=[
                    dependency("rusqlite", optional=True),
                    dependency("rusqlite", kind="dev"),
                ],
            )
        )

        owners = self.check.evaluate_storage_boundary(metadata)

        self.assertEqual(owners, self.check.STORAGE_OWNERS)

    def test_rejects_target_specific_non_owner_dependency(self) -> None:
        metadata = valid_metadata()
        metadata["packages"].append(
            package(
                "windows-wrapper",
                dependencies=[
                    dependency("rusqlite", target='cfg(target_os = "windows")')
                ],
            )
        )

        with self.assertRaisesRegex(
            self.check.StorageBoundaryViolation,
            "windows-wrapper",
        ):
            self.check.evaluate_storage_boundary(metadata)

    def test_rejects_production_forward_to_test_helpers(self) -> None:
        metadata = valid_metadata()
        metadata["packages"][0]["features"]["production"].append(
            "fixture-consumer/test-helpers"
        )
        metadata["packages"][0]["dependencies"].append(
            dependency("fixture-consumer")
        )
        metadata["packages"].append(
            package(
                "fixture-consumer",
                features={"test-helpers": ["dep:rusqlite"]},
                dependencies=[dependency("rusqlite", optional=True)],
            )
        )

        with self.assertRaisesRegex(
            self.check.StorageBoundaryViolation,
            "production features reach test-only helper surfaces: fixture-consumer",
        ):
            self.check.evaluate_storage_boundary(metadata)

    def test_rejects_stale_owner_allowlist_entry(self) -> None:
        metadata = valid_metadata()
        metadata["packages"] = [
            package
            for package in metadata["packages"]
            if package["name"] != "tracedecay-sessions"
        ]

        with self.assertRaisesRegex(
            self.check.StorageBoundaryViolation,
            "tracedecay-sessions",
        ):
            self.check.evaluate_storage_boundary(metadata)


if __name__ == "__main__":
    unittest.main()
