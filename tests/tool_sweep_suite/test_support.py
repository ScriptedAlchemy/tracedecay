"""Shared dynamic loaders for the MCP tool-sweep unit modules."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
from types import ModuleType


RUNNER_PATH = Path(__file__).with_name("runner.py")
SUITE_DIR = str(RUNNER_PATH.parent)
if SUITE_DIR not in sys.path:
    sys.path.insert(0, SUITE_DIR)


def _load(name: str, path: Path, purpose: str) -> ModuleType:
    if not path.is_file():
        raise AssertionError(f"missing {purpose}: {path}")
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise AssertionError(f"could not load {purpose}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def load_runner() -> ModuleType:
    return _load("tool_sweep_runner", RUNNER_PATH, "tool sweep runner")


def load_reports() -> ModuleType:
    return _load("tool_sweep_reports", RUNNER_PATH.with_name("reports.py"), "tool sweep reports")


def load_sweep() -> ModuleType:
    return _load("tool_sweep_sweep", RUNNER_PATH.with_name("sweep.py"), "tool sweep executor")


def load_orchestrator() -> ModuleType:
    return _load("tool_sweep_orchestrator", RUNNER_PATH.with_name("orchestrator.py"), "tool sweep orchestrator")


def load_journeys() -> ModuleType:
    return _load("tool_sweep_journeys", RUNNER_PATH.with_name("journeys.py"), "tool sweep journeys")
