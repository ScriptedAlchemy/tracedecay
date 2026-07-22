#!/usr/bin/env python3
"""Statically validate the PR9 production-acceptance packet."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any, NoReturn, cast


PACKET_FILE = "pr9-production-acceptance-v1.json"
REQUIRED_STATIC_GATES = {
    "packet_shape",
    "real_corpus_integrity",
    "production_api_bindings",
    "direct_contract_bindings",
    "promotion_pending",
}
REQUIRED_REQUIREMENTS = {
    "deterministic_extraction",
    "deterministic_generations",
    "restart_and_projection_replay",
    "exact_non_demotion",
    "lexical_ranking",
    "graph_join",
    "git_join",
    "diagnostic_join",
    "test_attribution_join",
    "coverage_and_abstention",
    "v1_parity",
    "platform_portability",
}
PARENT_GATE_COMMANDS = {
    "code_index_contracts": "cargo test --all-features --test code_index_suite",
    "search_quality_contracts": "cargo test --all-features --test search_quality_suite",
    "search_eval_cli_contracts": "cargo test --all-features --test search_eval_cli_test",
    "code_index_benchmark_validation": "cargo bench --bench code_index_chunks -- --validate-only",
    "platform_linux": "cargo test --all-features --test code_index_suite --test search_quality_suite",
    "platform_windows": "cargo test --all-features --test code_index_suite --test search_quality_suite",
    "platform_macos": "cargo test --all-features --test code_index_suite --test search_quality_suite",
}
REQUIRED_PLATFORMS = {"linux", "windows", "macos"}
SHA256_RE = re.compile(r"^sha256:([0-9a-f]{64})$")


def fail(message: str) -> NoReturn:
    raise SystemExit(f"invalid PR9 production-acceptance packet: {message}")


def load_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot load {path}: {error}")
    if not isinstance(value, dict):
        fail(f"{path} must contain an object")
    return cast(dict[str, Any], value)


def load_jsonl(path: Path) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    try:
        for line_number, line in enumerate(
            path.read_text(encoding="utf-8").splitlines(), start=1
        ):
            value = json.loads(line)
            if not isinstance(value, dict):
                fail(f"{path}:{line_number} must contain an object")
            records.append(cast(dict[str, Any], value))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot load {path}: {error}")
    if not records:
        fail(f"{path} must not be empty")
    return records


def repository_file(repository: Path, value: Any, name: str) -> Path:
    if not isinstance(value, str) or not value:
        fail(f"{name} must be a repository-relative path")
    relative = Path(value)
    if relative.is_absolute() or ".." in relative.parts:
        fail(f"{name} escapes the repository: {value}")
    path = repository / relative
    if not path.is_file():
        fail(f"{name} is missing: {value}")
    return path


def string_array(value: Any, name: str) -> list[str]:
    if (
        not isinstance(value, list)
        or not value
        or not all(isinstance(item, str) and item for item in value)
    ):
        fail(f"{name} must be a non-empty string array")
    return cast(list[str], value)


def sha256_digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def check_digest(path: Path, expected: Any, name: str) -> None:
    if not isinstance(expected, str) or SHA256_RE.fullmatch(expected) is None:
        fail(f"{name} must be a sha256 digest")
    actual = sha256_digest(path)
    if actual != expected:
        fail(f"{name} drifted: expected {expected}, got {actual}")


def check_packet_shape(packet: dict[str, Any]) -> None:
    if packet.get("schema_version") != 1:
        fail("schema_version must be 1")
    if packet.get("packet_id") != "pr9-production-acceptance-v1":
        fail("packet_id must identify the frozen PR9 packet")
    if packet.get("scope") != "production_acceptance_preflight":
        fail("scope must be production_acceptance_preflight")
    if set(string_array(packet.get("static_gate_ids"), "static_gate_ids")) != REQUIRED_STATIC_GATES:
        fail("static_gate_ids must exactly cover the validator")
    if set(string_array(packet.get("parent_gate_ids"), "parent_gate_ids")) != set(
        PARENT_GATE_COMMANDS
    ):
        fail("parent_gate_ids must exactly match the parent gate allowlist")

    requirements = packet.get("requirements")
    if not isinstance(requirements, list) or not requirements:
        fail("requirements must be a non-empty array")
    requirement_ids = {
        requirement.get("id")
        for requirement in requirements
        if isinstance(requirement, dict)
    }
    if requirement_ids != REQUIRED_REQUIREMENTS or len(requirements) != len(
        REQUIRED_REQUIREMENTS
    ):
        fail("requirements must exactly cover the PR9 acceptance matrix")

    platforms = packet.get("platforms")
    if not isinstance(platforms, list):
        fail("platforms must be an array")
    platform_ids = {
        platform.get("id") for platform in platforms if isinstance(platform, dict)
    }
    if platform_ids != REQUIRED_PLATFORMS or len(platforms) != len(REQUIRED_PLATFORMS):
        fail("platforms must exactly cover Linux, Windows, and macOS")
    for platform in platforms:
        if not isinstance(platform, dict):
            fail("each platform entry must be an object")
        if platform.get("status") != "pending_parent_gate":
            fail("platform evidence must remain pending until its parent gate runs")
        gate_id = platform.get("parent_gate_id")
        if gate_id != f"platform_{platform.get('id')}":
            fail("each platform must bind its matching parent gate")


def check_real_corpus_integrity(
    packet: dict[str, Any], repository: Path
) -> dict[str, dict[str, Any]]:
    manifest_path = repository_file(
        repository, packet.get("fixture_manifest"), "fixture manifest"
    )
    manifest = load_object(manifest_path)
    corpus = manifest.get("corpus")
    if not isinstance(corpus, list) or not corpus:
        fail("fixture manifest corpus must be non-empty")

    documents: dict[str, dict[str, Any]] = {}
    fixture_root = manifest_path.parent
    for entry in corpus:
        if not isinstance(entry, dict):
            fail("each corpus entry must be an object")
        document_id = entry.get("document_id")
        if not isinstance(document_id, str) or not document_id:
            fail("each corpus entry needs a document_id")
        if document_id in documents:
            fail(f"duplicate corpus document_id: {document_id}")
        snapshot = repository_file(
            fixture_root,
            entry.get("snapshot_path"),
            f"snapshot for {document_id}",
        )
        source = repository_file(
            repository,
            entry.get("source_repository_path"),
            f"checked-in source for {document_id}",
        )
        del source
        if snapshot.stat().st_size != entry.get("byte_len"):
            fail(f"snapshot byte length drifted for {document_id}")
        check_digest(
            snapshot,
            entry.get("content_digest"),
            f"snapshot digest for {document_id}",
        )
        documents[document_id] = entry

    artifacts = manifest.get("artifact_files")
    if not isinstance(artifacts, list) or not artifacts:
        fail("fixture manifest artifact_files must be non-empty")
    for artifact in artifacts:
        if not isinstance(artifact, dict):
            fail("each fixture artifact must be an object")
        path = repository_file(
            fixture_root, artifact.get("path"), "fixture artifact"
        )
        if path.stat().st_size != artifact.get("byte_len"):
            fail(f"fixture artifact byte length drifted: {artifact.get('path')}")
        check_digest(path, artifact.get("digest"), f"digest for {artifact.get('path')}")

    run_contract = repository_file(
        repository, packet.get("fixture_run_contract"), "fixture run contract"
    )
    load_object(run_contract)
    workload_path = repository_file(
        repository, packet.get("code_index_workload"), "code-index workload"
    )
    expected_path = repository_file(
        repository, packet.get("code_index_expected"), "code-index expected counts"
    )
    workload = load_object(workload_path)
    load_object(expected_path)
    workload_corpus = workload.get("corpus")
    if not isinstance(workload_corpus, dict):
        fail("code-index workload corpus must be an object")
    source_files = string_array(
        workload_corpus.get("source_files"), "code-index corpus source_files"
    )
    for source_file in source_files:
        repository_file(repository, source_file, "code-index corpus source")
    if not all(source_file.startswith("tests/fixtures/search_quality/corpus/") for source_file in source_files):
        fail("code-index workload must use the checked-in search-quality corpus")
    return documents


def check_production_api_bindings(
    packet: dict[str, Any], repository: Path
) -> None:
    requirements = cast(list[dict[str, Any]], packet["requirements"])
    for requirement in requirements:
        api_entries = requirement.get("production_apis")
        if not isinstance(api_entries, list) or not api_entries:
            fail(f"{requirement['id']} must bind production APIs")
        for api_entry in api_entries:
            if not isinstance(api_entry, dict):
                fail(f"{requirement['id']} production API entry must be an object")
            path = repository_file(
                repository,
                api_entry.get("path"),
                f"{requirement['id']} production API",
            )
            source = path.read_text(encoding="utf-8")
            for symbol in string_array(
                api_entry.get("symbols"), f"{requirement['id']} API symbols"
            ):
                if re.search(rf"\b{re.escape(symbol)}\b", source) is None:
                    fail(
                        f"{requirement['id']} production symbol {symbol} "
                        f"is absent from {path.relative_to(repository)}"
                    )


def check_direct_contract_bindings(
    packet: dict[str, Any],
    repository: Path,
    documents: dict[str, dict[str, Any]],
) -> None:
    parent_gates = set(PARENT_GATE_COMMANDS)
    requirements = cast(list[dict[str, Any]], packet["requirements"])
    for requirement in requirements:
        contract = requirement.get("direct_contract")
        if not isinstance(contract, dict):
            fail(f"{requirement['id']} direct_contract must be an object")
        path = repository_file(
            repository,
            contract.get("path"),
            f"{requirement['id']} direct contract",
        )
        source = path.read_text(encoding="utf-8")
        for test_name in string_array(
            contract.get("tests"), f"{requirement['id']} direct tests"
        ):
            if re.search(rf"\bfn\s+{re.escape(test_name)}\s*\(", source) is None:
                fail(
                    f"{requirement['id']} direct test {test_name} "
                    f"is absent from {path.relative_to(repository)}"
                )
        gate_id = requirement.get("parent_gate_id")
        if gate_id not in parent_gates:
            fail(f"{requirement['id']} references unknown parent gate {gate_id!r}")
        for document_id in string_array(
            requirement.get("corpus_documents"),
            f"{requirement['id']} corpus_documents",
        ):
            if document_id not in documents:
                fail(f"{requirement['id']} references unknown corpus document {document_id}")

    fixture_root = repository / "tests/fixtures/search_quality"
    judgments = load_jsonl(fixture_root / "judgments-development-v1.jsonl")
    abstention_values = {
        judgment.get("abstention_oracle") for judgment in judgments
    }
    if abstention_values != {False, True}:
        fail("development judgments must include answer and abstention oracles")
    queries = load_jsonl(fixture_root / "queries-v1.jsonl")
    if not any(query.get("family") == "expected_no_result" for query in queries):
        fail("query workload must include expected-no-result coverage")


def check_promotion_pending(packet: dict[str, Any], search_quality_root: Path) -> None:
    promotion = packet.get("promotion")
    if not isinstance(promotion, dict):
        fail("promotion must be an object")
    if promotion.get("status") != "pending_parent_gates":
        fail("promotion must remain pending_parent_gates")
    for field in ("runtime_results", "locked_report", "promotion_evidence"):
        if promotion.get(field) is not None:
            fail(f"promotion.{field} must remain null before parent gates")
    if promotion.get("requires_all_parent_gates") is not True:
        fail("promotion must require every parent gate")
    if promotion.get("requires_locked_outcome") != "accepted":
        fail("promotion must require a locked accepted outcome")
    if "outcome" in packet:
        fail("a static preflight packet cannot claim a terminal outcome")

    legacy_run = (
        search_quality_root
        / "runs"
        / "run-search-quality-contract-v1"
        / "revision-00000001"
    )
    legacy_files = [path for path in legacy_run.glob("*") if path.is_file()]
    if legacy_files:
        fail("obsolete contract-only run artifacts must not remain as PR9 evidence")


def validate(packet: dict[str, Any], repository: Path, search_quality_root: Path) -> None:
    check_packet_shape(packet)
    documents = check_real_corpus_integrity(packet, repository)
    check_production_api_bindings(packet, repository)
    check_direct_contract_bindings(packet, repository, documents)
    check_promotion_pending(packet, search_quality_root)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--list-parent-gates",
        action="store_true",
        help="print the allowlisted commands for the parent to run",
    )
    args = parser.parse_args()
    directory = Path(__file__).resolve().parent
    repository = directory.parents[1]
    packet = load_object(directory / PACKET_FILE)
    validate(packet, repository, directory)
    print(
        "valid PR9 production-acceptance preflight; "
        "promotion=pending_parent_gates"
    )
    if args.list_parent_gates:
        for gate_id in sorted(PARENT_GATE_COMMANDS):
            print(f"{gate_id}: {PARENT_GATE_COMMANDS[gate_id]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
