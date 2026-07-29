#!/usr/bin/env python3
"""Contracts for removing legacy packet gate-state orchestration."""

from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
LEGACY_HELPER = ROOT / "benchmarks" / "pr12_pr13_gate_evidence.py"
VALIDATORS = (
    ROOT / "benchmarks" / "pr13-host-conformance" / "validate_packet.py",
    ROOT / "benchmarks" / "pr13-advisory-milestone" / "validate_packet.py",
)
CI = ROOT / ".github" / "workflows" / "ci.yml"


class CiValidatorMigrationTest(unittest.TestCase):
    def test_legacy_gate_helper_and_references_are_removed(self) -> None:
        self.assertFalse(LEGACY_HELPER.exists())
        for path in (*VALIDATORS, CI):
            contents = path.read_text(encoding="utf-8")
            self.assertNotIn("pr12_pr13_gate_evidence", contents)
            self.assertNotIn("test_platform_evidence.py", contents)
            self.assertNotIn("validate_packet.py --strict", contents)

    def test_static_validators_still_pass_without_ephemeral_gate_state(self) -> None:
        for validator in VALIDATORS:
            with self.subTest(validator=validator):
                result = subprocess.run(
                    [sys.executable, str(validator)],
                    cwd=ROOT,
                    capture_output=True,
                    text=True,
                    check=False,
                    timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
