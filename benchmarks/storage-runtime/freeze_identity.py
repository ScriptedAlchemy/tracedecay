"""Frozen artifact identity capture and binding."""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

from runner_contract import (
    ConfigError, IDENTITY_ARTIFACT_ID, IDENTITY_SCHEMA_VERSION, RUNNER_VERSION, RunnerError,
)
from safe_paths import (
    _safe_mkdir_parents, artifact_fingerprint, assert_safe_path_components,
    atomic_write_json_new, create_fresh_directory, read_file_no_follow, validate_safe_tree,
)
from profile_safety import (
    build_child_env, create_child_sandbox, forbidden_profile_roots, guard_path, reject_network_filesystem,
)
from process_execution import (
    binary_identity, command_succeeded, preferred_output_summary, process_tree_capability,
    require_safe_identifier, run_command, safe_probe_base_env,
)
from evidence_validation import result_contains_absolute_paths

MAX_JSON_ARTIFACT_BYTES = 16 * 1024 * 1024


def frozen_product_commit(identity: dict) -> str:
    value = identity.get("product_commit_sha")
    if (
        not isinstance(value, str)
        or len(value) not in {40, 64}
        or any(character not in "0123456789abcdef" for character in value)
    ):
        raise ConfigError("frozen identity product_commit_sha is invalid")
    return value


def load_safe_json(path_like: str | Path, role: str) -> tuple[Path, dict]:
    path = assert_safe_path_components(path_like, role, require_directory=False)
    try:
        value = json.loads(
            read_file_no_follow(path, role, max_bytes=MAX_JSON_ARTIFACT_BYTES).decode("utf-8")
        )
    except (RunnerError, UnicodeError, json.JSONDecodeError) as exc:
        raise ConfigError(f"cannot load {role}: {type(exc).__name__}") from exc
    if not isinstance(value, dict):
        raise ConfigError(f"{role} must contain a JSON object")
    return path, value


def file_fingerprint(path_like: str | Path, role: str) -> dict[str, Any]:
    fingerprint = artifact_fingerprint(path_like, role)
    if fingerprint["kind"] != "file":
        raise ConfigError(f"{role} must be a regular file")
    return fingerprint


def freeze_version_probe(
    binary_path: Path,
    version_args: list[str],
    forbidden: list[tuple[str, Path]],
) -> dict[str, Any]:
    """Best-effort binary version metadata under the same isolated child policy."""
    if not version_args:
        return {"status": "not_requested"}
    if process_tree_capability()["state"] != "supported_best_effort":
        return {"status": "not_run_process_tree_unsupported"}
    with tempfile.TemporaryDirectory(prefix="tracedecay-s0-version-") as temporary:
        probe_root = create_fresh_directory(Path(temporary) / "probe", "freeze version probe")
        sandbox = create_child_sandbox(probe_root, "freeze version probe")
        env = build_child_env(
            safe_probe_base_env(dict(os.environ)), {}, [], forbidden, sandbox
        )
        try:
            result = run_command(
                [str(binary_path), *[str(arg) for arg in version_args]],
                env,
                30.0,
                cwd=sandbox["cwd"],
            )
            probe = {
                "status": "available" if command_succeeded(result) else "unavailable",
                "exit_code": result["exit_code"],
                "output": preferred_output_summary(result),
                "process_tree": result["process_tree"],
            }
        except RunnerError:
            probe = {"status": "unavailable"}
        validate_safe_tree(probe_root, "freeze version probe output")
        return probe


def _identity_component_match(expected: dict | None, actual: dict) -> bool:
    """Compare only immutable fingerprint fields; no path is an identity input."""
    if not isinstance(expected, dict):
        return False
    if expected.get("kind") != actual.get("kind"):
        return False
    if expected.get("kind") == "file":
        return (
            expected.get("sha256") == actual.get("sha256")
            and expected.get("size_bytes") == actual.get("size_bytes")
        )
    if expected.get("kind") == "tree":
        return (
            expected.get("aggregate_sha256") == actual.get("aggregate_sha256")
            and expected.get("file_count") == actual.get("file_count")
        )
    return False


def bind_frozen_identity(
    identity: dict,
    *,
    product_binary_path: str | Path,
    evidence_binary_path: str | Path,
    schema_manifest_path: str | Path,
    workload_path: Path,
    corpus_root: Path,
    config_path: str | Path,
) -> dict[str, Any]:
    """Fail closed unless every artifact tested by this run matches the freeze."""
    if identity.get("artifact_id") != IDENTITY_ARTIFACT_ID:
        raise ConfigError("frozen identity has an unsupported artifact_id")
    if identity.get("schema_version") != IDENTITY_SCHEMA_VERSION:
        raise ConfigError("frozen identity has an unsupported schema_version")
    product_commit_sha = frozen_product_commit(identity)
    expected = {
        "product_binary": identity.get("product_binary"),
        "evidence_binary": identity.get("evidence_binary"),
        "schema_manifest": identity.get("schema_manifest"),
        "workload": identity.get("workload"),
        "corpus": identity.get("corpus"),
        "config": identity.get("config"),
    }
    if any(not isinstance(value, dict) for value in expected.values()):
        raise ConfigError("frozen identity is missing one or more bound artifact fingerprints")
    product_binary = binary_identity(product_binary_path)
    evidence_binary = binary_identity(evidence_binary_path)
    if product_binary["sha256"] == evidence_binary["sha256"]:
        raise ConfigError("product and evidence binaries must be distinct artifacts")
    actual = {
        "product_binary": {"kind": "file", **product_binary},
        "evidence_binary": {"kind": "file", **evidence_binary},
        "schema_manifest": file_fingerprint(schema_manifest_path, "schema manifest"),
        "workload": file_fingerprint(workload_path, "workload"),
        "corpus": artifact_fingerprint(corpus_root, "corpus"),
        "config": artifact_fingerprint(config_path, "config"),
    }
    mismatches = [
        key
        for key, expected_value in expected.items()
        if not _identity_component_match(expected_value, actual[key])
    ]
    if mismatches:
        raise ConfigError(
            "frozen identity does not match tested artifacts: " + ", ".join(sorted(mismatches))
        )
    return {
        "status": "bound",
        "product_commit_sha": product_commit_sha,
        "components": {
            key: {
                "kind": value["kind"],
                "sha256": value.get("sha256", value.get("aggregate_sha256")),
                "size_bytes": value.get("size_bytes"),
                "file_count": value.get("file_count"),
                "verified": True,
            }
            for key, value in actual.items()
        },
    }


def bind_frozen_binaries(
    identity: dict,
    *,
    product_binary_path: str | Path,
    evidence_binary_path: str | Path,
) -> dict[str, dict[str, Any]]:
    """Bind the two executable roles without requiring unrelated freeze inputs."""
    if identity.get("artifact_id") != IDENTITY_ARTIFACT_ID:
        raise ConfigError("frozen identity has an unsupported artifact_id")
    if identity.get("schema_version") != IDENTITY_SCHEMA_VERSION:
        raise ConfigError("frozen identity has an unsupported schema_version")
    frozen_product_commit(identity)
    product = {"kind": "file", **binary_identity(product_binary_path)}
    evidence = {"kind": "file", **binary_identity(evidence_binary_path)}
    if product["sha256"] == evidence["sha256"]:
        raise ConfigError("product and evidence binaries must be distinct artifacts")
    for role, actual in (
        ("product_binary", product),
        ("evidence_binary", evidence),
    ):
        if not _identity_component_match(identity.get(role), actual):
            raise ConfigError(f"frozen identity does not match tested artifact: {role}")
    return {"product_binary": product, "evidence_binary": evidence}


def cmd_freeze(args: argparse.Namespace) -> int:
    home = Path.home()
    forbidden = forbidden_profile_roots(dict(os.environ), home)
    output_path = guard_path(args.output, "frozen identity output", forbidden)
    _safe_mkdir_parents(output_path.parent, "frozen identity output")
    assert_safe_path_components(
        output_path.parent, "frozen identity output", require_directory=True
    )

    product_binary_path = guard_path(args.product_binary, "product binary", forbidden)
    evidence_binary_path = guard_path(args.evidence_binary, "evidence binary", forbidden)
    schema_manifest_path = guard_path(args.schema_manifest, "schema manifest", forbidden)
    workload_path = guard_path(args.workload, "workload", forbidden)
    corpus_path = guard_path(args.corpus, "corpus", forbidden)
    config_path = guard_path(args.config, "config", forbidden)
    validate_safe_tree(corpus_path, "corpus")
    for path, role in (
        (product_binary_path, "product binary"),
        (evidence_binary_path, "evidence binary"),
        (schema_manifest_path, "schema manifest"),
        (workload_path, "workload"),
        (corpus_path, "corpus"),
        (config_path, "config"),
        (output_path.parent, "frozen identity output"),
    ):
        reject_network_filesystem(path, role)
    product_binary = {"kind": "file", **binary_identity(product_binary_path)}
    evidence_binary = {"kind": "file", **binary_identity(evidence_binary_path)}
    if product_binary["sha256"] == evidence_binary["sha256"]:
        raise ConfigError("product and evidence binaries must be distinct artifacts")
    product_commit_sha = args.product_commit_sha
    if (
        len(product_commit_sha) not in {40, 64}
        or any(character not in "0123456789abcdef" for character in product_commit_sha)
    ):
        raise ConfigError("product commit SHA must be a lowercase 40- or 64-digit identity")
    product_binary["version_probe"] = freeze_version_probe(
        product_binary_path, list(args.product_binary_version_argv or []), forbidden
    )
    evidence_binary["version_probe"] = {"status": "not_requested"}
    families = args.store_family or ["graph", "profile", "project", "session"]
    for family in families:
        require_safe_identifier(family, "store family")
    if len(set(families)) != len(families):
        raise ConfigError("store families must be unique")
    identity = {
        "artifact_id": IDENTITY_ARTIFACT_ID,
        "schema_version": IDENTITY_SCHEMA_VERSION,
        "captured_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "captured_by": f"run_storage_baseline.py {RUNNER_VERSION}",
        "product_commit_sha": product_commit_sha,
        "product_binary": product_binary,
        "evidence_binary": evidence_binary,
        "schema_manifest": file_fingerprint(schema_manifest_path, "schema manifest"),
        "workload": file_fingerprint(workload_path, "workload"),
        "corpus": artifact_fingerprint(corpus_path, "corpus"),
        "config": artifact_fingerprint(config_path, "config"),
        "store_families": families,
        "notes": args.notes or "",
    }
    identity_path_leaks = result_contains_absolute_paths(identity)
    if identity_path_leaks:
        raise ConfigError(
            "frozen identity notes/metadata may not contain absolute paths: "
            + ", ".join(identity_path_leaks)
        )
    atomic_write_json_new(
        output_path,
        identity,
        "frozen identity",
        indent=2,
        ensure_ascii=True,
    )
    print(f"[s0] frozen identity written to {output_path}", file=sys.stderr)
    return 0
