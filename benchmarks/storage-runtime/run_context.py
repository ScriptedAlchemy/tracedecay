"""Per-run isolated state and command materialization."""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

from runner_contract import ConfigError, ExecutionError, SafetyError
from safe_paths import (
    _path_is_within, artifact_fingerprint, copy_safe_artifact, copy_safe_tree,
    create_fresh_directory, fingerprint_tree,
)
from profile_safety import build_child_env, create_child_sandbox
from process_execution import (
    binary_identity, run_command, safe_expanded_path, substitute, substitute_argv,
)
from workload_model import _config_number, _fingerprint_matches_bound

class RunContext:
    def __init__(
        self,
        workload: dict,
        input_root: Path,
        output_root: Path,
        base_env: dict,
        forbidden: list[tuple[str, Path]],
        timeout_default: float,
        product_binary: str | None,
        evidence_binary: str | None,
        config_source: Path | None = None,
        bound_corpus: dict[str, Any] | None = None,
        bound_product_binary: dict[str, Any] | None = None,
        bound_evidence_binary: dict[str, Any] | None = None,
        bound_config: dict[str, Any] | None = None,
    ):
        self.workload = workload
        self.input_root = input_root
        self.output_root = output_root
        self.base_env = base_env
        self.forbidden = forbidden
        self.timeout_default = timeout_default
        self.product_binary = product_binary
        self.evidence_binary = evidence_binary
        self.config_source = config_source
        self.bound_corpus = bound_corpus
        self.bound_product_binary = bound_product_binary
        self.bound_evidence_binary = bound_evidence_binary
        self.bound_config = bound_config
        self.phase_evidence: dict[tuple[str, str], dict[str, Any]] = {}
        self.phase_run_dirs: dict[tuple[str, str], Path] = {}
        self.run_state: dict[Path, dict[str, Path]] = {}
        self.runs: list[dict] = []
        self.work_root = create_fresh_directory(output_root / "work", "runner work")

    def _owned_directory(self, path: Path, role: str) -> Path:
        if path.exists():
            safe_expanded_path(str(path), self.output_root, role, require_directory=True)
            return path
        safe_expanded_path(str(path), self.output_root, role)
        return create_fresh_directory(path, role)

    def prepare_run(self, run_dir: Path, phase: dict, family: str) -> None:
        store = copy_safe_tree(
            self.input_root, run_dir / "store", f"phase {phase['name']} family {family}"
        )
        sandbox = create_child_sandbox(run_dir, "child", data_root=store)
        if self.config_source is not None:
            copied_config = copy_safe_artifact(
                self.config_source, sandbox["config"], "frozen config"
            )
            if self.bound_config is not None and not _fingerprint_matches_bound(
                artifact_fingerprint(copied_config, "runner-owned config copy"),
                self.bound_config,
            ):
                raise SafetyError("runner-owned config copy does not match frozen identity")
        if self.bound_corpus is not None:
            copied_fingerprint = {
                "kind": "tree",
                **fingerprint_tree(store, "runner-owned corpus copy"),
            }
            if not _fingerprint_matches_bound(
                copied_fingerprint,
                self.bound_corpus,
            ):
                raise SafetyError("runner-owned corpus copy no longer matches frozen identity")
        self.run_state[run_dir] = {"store": store, **sandbox}

    def state(self, run_dir: Path) -> dict[str, Path]:
        try:
            return self.run_state[run_dir]
        except KeyError as exc:
            raise ExecutionError("run directory was not initialized by the runner") from exc

    def mapping(self, family: str, run_dir: Path, repetition: int = 0) -> dict[str, str]:
        state = self.state(run_dir)
        return {
            "INPUT": str(state["store"]),
            "OUTPUT": str(state["output"]),
            "RUN_DIR": str(run_dir),
            "FAMILY": family,
            "PRODUCT_BINARY": self.product_binary or "",
            "EVIDENCE_BINARY": self.evidence_binary or "",
            "PYTHON": sys.executable,
            "REPETITION": str(repetition),
            "HOME": str(state["home"]),
            "CONFIG": str(state["config"]),
            "CACHE": str(state["cache"]),
            "TRACEDECAY_DATA_DIR": str(state["data"]),
            "TRACEDECAY_GLOBAL_DB": str(state["data"] / "global.db"),
        }

    def path_roots(self, run_dir: Path) -> dict[str, Path]:
        state = self.state(run_dir)
        roots = {
            "INPUT": state["store"],
            "OUTPUT": state["output"],
            "RUN_DIR": run_dir,
            "HOME": state["home"],
            "CONFIG": state["config"],
            "CACHE": state["cache"],
            "TRACEDECAY_DATA_DIR": state["data"],
            "TRACEDECAY_GLOBAL_DB": state["data"],
        }
        if self.product_binary:
            roots["PRODUCT_BINARY"] = Path(self.product_binary).parent
        if self.evidence_binary:
            roots["EVIDENCE_BINARY"] = Path(self.evidence_binary).parent
        return roots

    def child_env(self, run_dir: Path) -> dict[str, str]:
        safety = self.workload.get("safety", {})
        return build_child_env(
            self.base_env,
            dict(safety.get("env") or {}),
            list(safety.get("env_path_keys") or []),
            self.forbidden,
            self.state(run_dir),
        )

    def command(self, step: dict, family: str, run_dir: Path, repetition: int = 0) -> dict:
        if self.product_binary and self.bound_product_binary is not None:
            current = binary_identity(self.product_binary)
            if (
                current["sha256"] != self.bound_product_binary.get("sha256")
                or current["size_bytes"] != self.bound_product_binary.get("size_bytes")
            ):
                raise SafetyError("tested product binary changed after frozen identity binding")
        if self.evidence_binary and self.bound_evidence_binary is not None:
            current = binary_identity(self.evidence_binary)
            if (
                current["sha256"] != self.bound_evidence_binary.get("sha256")
                or current["size_bytes"] != self.bound_evidence_binary.get("size_bytes")
            ):
                raise SafetyError("evidence binary changed after frozen identity binding")
        argv = substitute_argv(
            step["argv"], self.mapping(family, run_dir, repetition), self.path_roots(run_dir)
        )
        return run_command(
            argv, self.child_env(run_dir), self.timeout(step), cwd=self.state(run_dir)["cwd"]
        )

    def expand_path(self, template: object, family: str, run_dir: Path, role: str) -> Path:
        template_text = str(template)
        mapping = self.mapping(family, run_dir)
        value = substitute(template_text, mapping)
        roots = [
            root
            for token, root in self.path_roots(run_dir).items()
            if f"__{token}__" in template_text
        ]
        if not roots:
            raise ConfigError(f"{role} must use a runner-owned path placeholder")
        # A path cannot safely span independent roots.  Multiple placeholders
        # are permitted only when they still resolve under the same root.
        for root in roots:
            if _path_is_within(value, root):
                return safe_expanded_path(value, root, role)
        raise SafetyError(f"{role} does not remain inside its declared placeholder root")

    def timeout(self, step: dict) -> float:
        defaults = self.workload.get("defaults", {})
        return _config_number(
            step.get("timeout_seconds", defaults.get("timeout_seconds", self.timeout_default)),
            "command timeout_seconds",
            0,
            strict=True,
        )
