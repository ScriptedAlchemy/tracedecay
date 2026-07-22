#!/usr/bin/env python3
"""Validate the locked-but-pending PR10 semantic search evaluation packet."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import tomllib
from pathlib import Path
from typing import Any, NoReturn, cast


EXPECTED_PROFILES = {
    "pr9-exact-lexical-graph",
    "semantic-ann",
    "semantic-exact-flat",
    "semantic-hybrid",
    "semantic-late-interaction",
    "semantic-quantized",
    "semantic-reranked",
}
EXPECTED_PARENT_GATES = {
    "accepted_pr9_locked_baseline",
    "aggregate_all_feature_gate",
    "asynchronous_search_during_indexing",
    "atomic_compatible_generation_activation",
    "calibration_and_abstention",
    "cold_offline_rollback",
    "current_and_10x_resources",
    "fallback_byte_stability",
    "library_first_default_all_features",
    "linux_and_windows_native_runtime",
    "local_verified_model_bytes",
    "plan15_locked_holdout_decision",
    "production_exact_flat_vector_service",
    "production_fastembed_runtime",
    "saved_candidate_ablation_evidence",
}
EXPECTED_FAILURE_CLASSES = {
    "cancelled",
    "corrupt_artifact",
    "failed_generation",
    "incompatible_generation",
    "indexing",
    "invalid_calibration",
    "integrity_mismatch",
    "missing_artifact",
    "out_of_memory",
    "reranker_failure",
    "stale_generation",
    "timeout",
    "unavailable",
}
RESEARCH_CANDIDATES = {
    "semantic-ann",
    "semantic-late-interaction",
    "semantic-quantized",
}
EXPECTED_AUDIT_REQUIREMENTS = {
    "asynchronous_non_blocking_indexing",
    "byte_stable_pr9_fallback",
    "calibrated_abstention",
    "default_equals_all_features",
    "exact_flat_baseline",
    "immutable_generations",
    "library_first_fastembed",
    "local_verified_model_bytes",
    "locked_evidence_before_activation",
}
EXPECTED_CALLABLE_ACCEPTANCE: dict[str, tuple[str, str, tuple[str, ...]]] = {
    "search_during_indexing": (
        "tests/pr10_vector_generation_prep_test.rs",
        "indexing_and_cancellation_leave_only_the_compatible_prior_generation_queryable",
        (
            "tokio::spawn(prepare_vector_generation_async(",
            "active_generation_for(",
            ".is_some()",
            ".is_none()",
            "cancel_generation(",
            "VectorGenerationStoreErrorV1::UnknownBuild",
        ),
    ),
    "atomic_generation_publication": (
        "tests/pr10_vector_generation_prep_test.rs",
        "checkpoint_and_active_pointer_publish_atomically",
        (
            "fail_before_publication_swap_once(",
            "VectorGenerationStoreErrorV1::InjectedPublicationFailure",
            "active_generation_id()",
            "publication.generation_id",
        ),
    ),
    "runtime_routes_only_current_generation": (
        "src/application/semantic_runtime/tests.rs",
        "only_a_current_receipt_routes_to_semantic_search",
        (
            "SemanticRuntimeStateV1::Current",
            "SemanticRuntimeStateV1::Indexing",
            "SemanticRuntimeStateV1::Degraded",
            "SemanticRuntimeStateV1::Rollback",
            "SemanticRuntimeRouteV1::LexicalFallback",
        ),
    ),
    "indexing_receipt_cannot_activate": (
        "src/application/semantic_runtime/tests.rs",
        "activation_receipt_cannot_silently_promote_an_indexing_runtime",
        (
            ".activate(",
            "SemanticRuntimeControlErrorV1::PromotionNotObserved",
            "SemanticRuntimeRouteV1::LexicalFallback",
        ),
    ),
    "semantic_capacity_never_blocks_query": (
        "src/semantic_code/runtime_query.rs",
        "saturated_runtime_omits_semantics_without_entering_the_waiter_queue",
        (
            "service.acquire()",
            ".embed_query(",
            "RetrievalPortError::AuthorityUnavailable",
            "service.stats().queued_waiters",
        ),
    ),
    "exact_flat_callable": (
        "src/query/retrieval/semantic/tests.rs",
        "exact_flat_scan_is_deterministic_and_emits_generic_semantic_evidence",
        (
            "Retriever::<SemanticRetrievalRequestV1",
            "SemanticSearchKindV1::ExactFlat",
            "evidence.vector_generation",
        ),
    ),
    "calibrated_fallback_and_strict_unavailable": (
        "src/query/retrieval/semantic/tests.rs",
        "missing_or_shifted_calibration_abstains_and_preserves_exact_fallback",
        (
            "SemanticQueryModeV1::FallbackAllowed",
            "Arc::as_ptr(permissive.fallback())",
            "embedder.calls.get(), 0",
            "SemanticQueryModeV1::StrictSemantic",
            "SemanticQueryServiceError::StrictUnavailable",
        ),
    ),
    "calibrated_threshold_abstention": (
        "src/query/retrieval/semantic/tests.rs",
        "calibrated_distance_and_margin_thresholds_abstain_without_relabeling_scores",
        ("SemanticAbstentionV1::AmbiguousTopCandidates",),
    ),
}


class PacketError(ValueError):
    """A deterministic packet validation failure."""


def fail(message: str) -> NoReturn:
    raise PacketError(message)


def load_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot load {path}: {error}")
    if not isinstance(value, dict):
        fail(f"{path} must contain an object")
    return cast(dict[str, Any], value)


def repository_path(repository: Path, value: Any, field: str) -> Path:
    if not isinstance(value, str) or not value:
        fail(f"{field} must be a non-empty repository-relative path")
    relative = Path(value)
    if relative.is_absolute() or ".." in relative.parts:
        fail(f"{field} escapes the repository")
    path = repository / relative
    if not path.is_file():
        fail(f"{field} does not exist: {value}")
    return path


def digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def rust_function_body(source: str, function: str, after: str | None = None) -> str:
    start = 0
    if after is not None:
        start = source.find(after)
        if start < 0:
            fail(f"Rust source is missing implementation anchor: {after}")
    pattern = re.compile(
        rf"\b(?:async\s+)?fn\s+{re.escape(function)}\s*(?:<[^{{;]*>)?\s*\("
    )
    match = pattern.search(source, start)
    if match is None:
        fail(f"Rust source is missing callable function: {function}")
    opening = source.find("{", match.end())
    if opening < 0:
        fail(f"Rust callable has no body: {function}")

    depth = 0
    index = opening
    state = "code"
    while index < len(source):
        char = source[index]
        next_char = source[index + 1] if index + 1 < len(source) else ""
        if state == "code":
            if char == "/" and next_char == "/":
                state = "line_comment"
                index += 2
                continue
            if char == "/" and next_char == "*":
                state = "block_comment"
                index += 2
                continue
            if char == '"':
                state = "string"
            elif char == "'":
                state = "char"
            elif char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                if depth == 0:
                    return source[opening : index + 1]
        elif state == "line_comment":
            if char == "\n":
                state = "code"
        elif state == "block_comment":
            if char == "*" and next_char == "/":
                state = "code"
                index += 2
                continue
        elif state in {"string", "char"}:
            if char == "\\":
                index += 2
                continue
            if (state == "string" and char == '"') or (state == "char" and char == "'"):
                state = "code"
        index += 1
    fail(f"Rust callable has an unterminated body: {function}")


def require_body_tokens(
    source: str,
    function: str,
    required: tuple[str, ...],
    after: str | None = None,
) -> None:
    body = rust_function_body(source, function, after)
    for token in required:
        if token not in body:
            fail(f"callable {function} does not exercise required behavior: {token}")


def verify_artifact(
    repository: Path,
    artifact: dict[str, Any],
    field: str,
) -> None:
    path = repository_path(repository, artifact.get("path"), f"{field}.path")
    expected_length = artifact.get("byte_len")
    if not isinstance(expected_length, int) or expected_length < 0:
        fail(f"{field}.byte_len must be a non-negative integer")
    if path.stat().st_size != expected_length:
        fail(
            f"{field} byte length drifted: expected {expected_length}, "
            f"found {path.stat().st_size}"
        )
    expected_digest = artifact.get("digest")
    if digest(path) != expected_digest:
        fail(f"{field} digest drifted")


def validate_corpus(workload: dict[str, Any], repository: Path) -> None:
    corpus = workload.get("corpus")
    if not isinstance(corpus, dict):
        fail("corpus must be an object")
    if corpus.get("provider") != "checked_in_real_repo_fixture":
        fail("corpus must use the checked-in real repository fixture")

    for field in ("fixture_manifest", "query_set", "development_labels"):
        artifact = corpus.get(field)
        if not isinstance(artifact, dict):
            fail(f"corpus.{field} must be an artifact object")
        verify_artifact(repository, artifact, f"corpus.{field}")

    files = corpus.get("files")
    if not isinstance(files, list) or len(files) != corpus.get("file_count"):
        fail("corpus.files must match corpus.file_count")
    total_bytes = 0
    for index, artifact in enumerate(files):
        if not isinstance(artifact, dict):
            fail(f"corpus.files[{index}] must be an object")
        verify_artifact(repository, artifact, f"corpus.files[{index}]")
        total_bytes += cast(int, artifact["byte_len"])
    if total_bytes != corpus.get("total_bytes"):
        fail("corpus.total_bytes does not match pinned file bytes")

    holdout = corpus.get("holdout")
    if not isinstance(holdout, dict):
        fail("corpus.holdout must be an object")
    if holdout.get("access") != "sealed_until_frozen_parent_run":
        fail("holdout must remain sealed until the frozen parent run")


def validate_production_apis(workload: dict[str, Any], repository: Path) -> None:
    production_apis = workload.get("production_apis")
    if not isinstance(production_apis, dict):
        fail("production_apis must be an object")
    if set(production_apis) != {"fastembed_runtime", "vector_service"}:
        fail("production_apis must name only the FastEmbed and vector boundaries")

    runtime = production_apis["fastembed_runtime"]
    vector = production_apis["vector_service"]
    if not isinstance(runtime, dict) or not isinstance(vector, dict):
        fail("production API descriptors must be objects")
    if any("required_symbols" in descriptor for descriptor in (runtime, vector)):
        fail("production API acceptance must name callable functions, not symbol scaffolding")

    runtime_source = repository_path(
        repository, runtime.get("path"), "production_apis.fastembed_runtime.path"
    ).read_text(encoding="utf-8")
    expected_runtime_functions = [
        {
            "implementation": "impl EmbeddingRuntime for FastEmbedEmbeddingRuntime",
            "function": "open_session",
        },
        {
            "implementation": "production FastEmbed local-byte model constructor",
            "function": "fastembed_model",
        },
    ]
    if runtime.get("callable_functions") != expected_runtime_functions:
        fail("FastEmbed runtime must bind the production local-byte callables")
    require_body_tokens(
        runtime_source,
        "open_session",
        (
            "self.verify_artifact_compatibility(authority)?",
            "fastembed_model(artifact)?",
            "TextEmbedding::try_new_from_user_defined",
        ),
        after="impl EmbeddingRuntime for FastEmbedEmbeddingRuntime",
    )
    require_body_tokens(
        runtime_source,
        "fastembed_model",
        (
            "ArtifactMemberRoleV1::Model",
            "ArtifactMemberRoleV1::Tokenizer",
            "ArtifactMemberRoleV1::Config",
            "UserDefinedEmbeddingModel::new",
        ),
    )

    vector_source = repository_path(
        repository, vector.get("path"), "production_apis.vector_service.path"
    ).read_text(encoding="utf-8")
    if vector.get("callable_functions") != [
        {
            "implementation": "impl Retriever<SemanticRetrievalRequestV1, CodeSemanticEvidenceV1> for SemanticCodeRetriever",
            "function": "retrieve",
        }
    ]:
        fail("vector service must bind the production Retriever callable")
    require_body_tokens(
        vector_source,
        "retrieve",
        ("self.retrieve_semantic(request)",),
        after="Retriever<SemanticRetrievalRequestV1",
    )
    require_body_tokens(
        vector_source,
        "retrieve_complete",
        (
            "self.vectors.scan_exact_flat",
            "SemanticSearchKindV1::ExactFlat",
            "RetrieverOutcome::Complete",
        ),
    )


def validate_feature_contract(repository: Path) -> None:
    manifest_path = repository / "Cargo.toml"
    try:
        manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        fail(f"cannot parse root Cargo feature manifest: {error}")
    features = manifest.get("features")
    dependencies = manifest.get("dependencies")
    if not isinstance(features, dict) or not isinstance(dependencies, dict):
        fail("root Cargo manifest must define features and dependencies")
    default = features.get("default")
    if not isinstance(default, list) or not all(isinstance(value, str) for value in default):
        fail("root default features must be a string array")
    declared = set(features) - {"default"}
    if len(default) != len(set(default)) or set(default) != declared:
        fail("root default feature set must exactly equal all declared root features")
    fastembed = dependencies.get("fastembed")
    if not isinstance(fastembed, dict):
        fail("FastEmbed must be a structured root dependency")
    if fastembed.get("optional") is not True or fastembed.get("default-features") is not False:
        fail("FastEmbed must be optional with upstream default features disabled")
    hf_hub = dependencies.get("hf-hub")
    if not isinstance(hf_hub, dict):
        fail("hf-hub must be a structured root dependency")
    if hf_hub.get("optional") is not True or hf_hub.get("default-features") is not False:
        fail("hf-hub acquisition must be optional with upstream default features disabled")
    semantic_feature = features.get("semantic-fastembed")
    if semantic_feature != [
        "dep:fastembed",
        "dep:hf-hub",
        "fastembed/ort-download-binaries-rustls-tls",
        "fastembed/hf-hub-rustls-tls",
    ]:
        fail("semantic-fastembed must select the pinned FastEmbed, acquisition, and native ORT stack")


def validate_model_integrity(workload: dict[str, Any], repository: Path) -> None:
    contract = workload.get("model_integrity")
    if not isinstance(contract, dict):
        fail("model_integrity must be an object")
    expected = {
        "artifact_contract": "versioned_manifest_with_sha256_length_and_member_digests",
        "runtime_input": "locally_installed_verified_member_bytes",
        "fastembed_constructor": "TextEmbedding::try_new_from_user_defined",
        "fastembed_upstream_default_features": False,
        "ambient_hub_discovery": False,
        "ambient_cache_discovery": False,
        "query_time_download": False,
        "network_inference": False,
        "external_inference_process": False,
    }
    if contract != expected:
        fail("model_integrity must pin local versioned-manifest SHA-256 behavior")

    artifact_source = (repository / "src/semantic_code/artifact_store.rs").read_text(
        encoding="utf-8"
    )
    require_body_tokens(
        artifact_source,
        "read_member_bytes",
        (
            "metadata.len() != member.byte_length",
            "Sha256DigestHex::of_bytes(&bytes) != member.digest",
            "AdmittedArtifactReadErrorV1::Corrupt",
        ),
        after="impl AdmittedArtifactSourceV1",
    )
    runtime_source = (repository / "src/semantic_code/fastembed_adapter.rs").read_text(
        encoding="utf-8"
    )
    production_source = runtime_source.split("#[cfg(test)]", 1)[0]
    for forbidden in (
        "hf_hub::",
        "HUGGINGFACE_HUB_CACHE",
        "HF_HOME",
        "HF_ENDPOINT",
        "FASTEMBED_CACHE_DIR",
        "TextEmbedding::try_new(",
    ):
        if forbidden in production_source:
            fail(f"production FastEmbed runtime uses ambient model surface: {forbidden}")


def validate_async_activation(workload: dict[str, Any]) -> None:
    contract = workload.get("asynchronous_activation")
    if not isinstance(contract, dict):
        fail("asynchronous_activation must be an object")
    if contract.get("projection_execution") != "background_optional_work":
        fail("semantic projection must be background optional work")
    if contract.get("baseline_lanes") != ["exact_literal", "lexical", "graph"]:
        fail("search-during-indexing must preserve exact, lexical, and graph lanes")
    if contract.get("baseline_waits_for_semantic") is not False:
        fail("baseline lanes cannot wait for semantic work")
    if contract.get("semantic_visibility") != "complete_compatible_generation_atomically_current":
        fail("semantic visibility must require one complete compatible current generation")
    if set(contract.get("excluded_generation_states", [])) != {
        "partial",
        "indexing",
        "stale",
        "failed",
        "cancelled",
        "incompatible",
    }:
        fail("every non-current semantic generation state must be excluded")
    if contract.get("excluded_states_affect_rank") is not False:
        fail("excluded semantic generations cannot affect rank")
    if contract.get("strict_semantic_behavior") != "typed_unavailable":
        fail("strict semantic must return typed unavailable when semantics are absent")
    if contract.get("search_during_indexing_comparison") != "byte_identical_pr9_fallback_and_rank":
        fail("search during indexing must compare PR9 fallback bytes and rank")
    if contract.get("activation_visibility") != "generation_and_active_pointer_single_atomic_step":
        fail("semantic generation and active pointer must become visible atomically")


def validate_implementation_audit(workload: dict[str, Any]) -> None:
    entries = workload.get("implementation_audit")
    if not isinstance(entries, list):
        fail("implementation_audit must be an array")
    by_id = {
        entry.get("id"): entry
        for entry in entries
        if isinstance(entry, dict) and isinstance(entry.get("id"), str)
    }
    if set(by_id) != EXPECTED_AUDIT_REQUIREMENTS or len(entries) != len(by_id):
        fail("implementation audit must cover every binding PR10 requirement once")
    for requirement, entry in by_id.items():
        if entry.get("locked_acceptance") != "pending_parent_execution":
            fail(f"{requirement} must remain pending locked parent acceptance")
        expected_delivery = (
            "activation_guard_delivered_evidence_consumption_pending"
            if requirement == "locked_evidence_before_activation"
            else "delivered"
        )
        if entry.get("delivery") != expected_delivery:
            fail(f"{requirement} delivery audit is inaccurate")


def validate_callable_acceptance(workload: dict[str, Any], repository: Path) -> None:
    entries = workload.get("callable_acceptance")
    if not isinstance(entries, list):
        fail("callable_acceptance must be an array")
    by_id = {
        entry.get("id"): entry
        for entry in entries
        if isinstance(entry, dict) and isinstance(entry.get("id"), str)
    }
    if set(by_id) != set(EXPECTED_CALLABLE_ACCEPTANCE) or len(entries) != len(by_id):
        fail("callable acceptance must cover every PR10 direct behavior once")
    for contract_id, (path, function, required) in EXPECTED_CALLABLE_ACCEPTANCE.items():
        entry = by_id[contract_id]
        if entry.get("path") != path or entry.get("function") != function:
            fail(f"{contract_id} must bind its direct callable regression")
        source = repository_path(
            repository, entry.get("path"), f"callable_acceptance.{contract_id}.path"
        ).read_text(encoding="utf-8")
        require_body_tokens(source, function, required)


def validate_plan_contract(repository: Path) -> None:
    plan31 = (
        repository / "docs/plans/tracedecay-v2/31-native-fastembed-semantic-code-search.md"
    ).read_text(encoding="utf-8")
    plan15 = (
        repository
        / "docs/plans/tracedecay-v2/15-search-quality-evaluation-and-retrieval-research.md"
    ).read_text(encoding="utf-8")
    combined = f"{plan31}\n{plan15}"
    for forbidden in (
        r"\bsignature\b",
        r"\btrust[- ]root\b",
        r"\bsigned (?:artifact|embedding|manifest|model|profile|report|sealed)\b",
        r"\brevoked (?:artifact|signature)\b",
        r"\battestation\b",
    ):
        if re.search(forbidden, combined, flags=re.IGNORECASE):
            fail(f"plans retain obsolete custom artifact-security machinery: {forbidden}")

    required_by_plan = {
        "Plan 31": (
            plan31,
            (
                "Semantic projection and indexing run asynchronously.",
                "Existing exact, lexical,\nand graph operations remain callable",
                "versioned canonical manifest",
                "SHA-256",
                "## Current delivered-artifact audit",
            ),
        ),
        "Plan 15": (
            plan15,
            (
                "Semantic projection and indexing are asynchronous optional work.",
                "FastEmbed is library-first",
                "complete compatible immutable generation is atomically current",
                "Strict\n  semantic alone may return typed unavailable",
            ),
        ),
    }
    for plan_name, (content, required_phrases) in required_by_plan.items():
        for phrase in required_phrases:
            if phrase not in content:
                fail(f"{plan_name} is missing binding requirement: {phrase}")


def validate_profiles(workload: dict[str, Any]) -> None:
    profiles = workload.get("profiles")
    if not isinstance(profiles, list):
        fail("profiles must be an array")
    by_id: dict[str, dict[str, Any]] = {}
    for profile in profiles:
        if not isinstance(profile, dict) or not isinstance(profile.get("id"), str):
            fail("every profile must be an identified object")
        profile_id = cast(str, profile["id"])
        if profile_id in by_id:
            fail(f"duplicate profile: {profile_id}")
        by_id[profile_id] = profile
    if set(by_id) != EXPECTED_PROFILES:
        fail("profile matrix must exactly cover baseline, oracle, hybrid, rerank, and research candidates")

    oracle = by_id["semantic-exact-flat"]
    if (
        oracle.get("role") != "production_baseline_and_oracle"
        or oracle.get("production_api") != "SemanticVectorReadPort::scan_exact_flat"
        or oracle.get("search_kind") != "exact_flat"
    ):
        fail("semantic-exact-flat must be the production baseline and exact-flat oracle")
    if oracle.get("activation_eligible") is not False:
        fail("the exact-flat oracle cannot activate before parent acceptance")

    for profile_id in RESEARCH_CANDIDATES:
        profile = by_id[profile_id]
        if (
            profile.get("state") != "candidate_evidence_required"
            or profile.get("activation_eligible") is not False
        ):
            fail(f"{profile_id} must remain an evidence-gated candidate")

    candidate_budgets = {
        profile.get("candidate_budget_per_lane")
        for profile in profiles
        if "candidate_budget_per_lane" in profile
    }
    if candidate_budgets != {32}:
        fail("all channel ablations must use the same candidate budget")


def validate_quality_contract(workload: dict[str, Any]) -> None:
    calibration = workload.get("calibration")
    if not isinstance(calibration, dict):
        fail("calibration must be an object")
    if calibration.get("raw_similarity_is_confidence") is not False:
        fail("raw similarity cannot be treated as confidence")
    if (
        calibration.get("invalid_or_shifted_behavior")
        != "abstain_and_report_invalid_calibration"
    ):
        fail("invalid or shifted calibration must visibly abstain")

    fallback = workload.get("fallback")
    if not isinstance(fallback, dict):
        fail("fallback must be an object")
    if fallback.get("comparison") != "byte_identical_pr9_fallback_subpayload":
        fail("fallback comparison must be byte-identical")
    if set(fallback.get("semantic_failure_classes", [])) != EXPECTED_FAILURE_CLASSES:
        fail("fallback failure classes are incomplete")
    if fallback.get("expected_digest") is not None:
        fail("fallback digest must remain null until the parent gate executes")


def validate_resource_and_rollback_contract(workload: dict[str, Any]) -> None:
    strata = workload.get("resource_strata")
    if not isinstance(strata, list) or len(strata) != 2:
        fail("resource_strata must contain current and 10x")
    by_scale = {
        str(stratum.get("scale")): stratum
        for stratum in strata
        if isinstance(stratum, dict)
    }
    if set(by_scale) != {"current", "10x"}:
        fail("resource_strata must contain current and 10x")
    current = by_scale["current"]
    ten_x = by_scale["10x"]
    for field in ("file_count", "eligible_chunks"):
        current_value = current.get(field)
        ten_x_value = ten_x.get(field)
        if not isinstance(current_value, int) or ten_x_value != current_value * 10:
            fail(f"10x {field} must be exactly ten times current")
    for stratum in strata:
        if stratum.get("warmups") != 5 or stratum.get("measured_repetitions") != 30:
            fail("each resource stratum must retain 5 warmups and 30 samples")
        if stratum.get("concurrency") != [1, "declared_saturation"]:
            fail("resource strata must cover concurrency 1 and declared saturation")

    platforms = workload.get("platform_strata")
    if not isinstance(platforms, list):
        fail("platform_strata must be an array")
    if {platform.get("platform") for platform in platforms} != {"linux", "windows"}:
        fail("native platform strata must cover Linux and Windows")
    if any(platform.get("runtime") != "native_fastembed" for platform in platforms):
        fail("platform strata must use native FastEmbed")

    metrics = workload.get("resource_metrics")
    if not isinstance(metrics, list) or not {
        "baseline_search_during_indexing_ns",
        "atomic_activation_visibility_ns",
        "baseline_rank_drift_during_indexing",
    }.issubset(set(metrics)):
        fail("resource metrics must measure search during indexing and atomic activation")

    rollback = workload.get("rollback")
    if not isinstance(rollback, dict):
        fail("rollback must be an object")
    if rollback.get("cold_start") is not True or rollback.get("offline") is not True:
        fail("rollback must prove a cold offline start")
    if rollback.get("on_failure") != "semantic_disabled":
        fail("rollback failure must leave semantics disabled")


def validate_pending_acceptance(
    workload: dict[str, Any],
    result: dict[str, Any],
) -> None:
    gates = workload.get("parent_gates")
    if not isinstance(gates, list):
        fail("parent_gates must be an array")
    gate_states = {
        gate.get("id"): gate.get("state")
        for gate in gates
        if isinstance(gate, dict)
    }
    if set(gate_states) != EXPECTED_PARENT_GATES:
        fail("parent gate set is incomplete")
    if set(gate_states.values()) != {"pending"}:
        fail("checked-in packet cannot claim a parent gate passed")

    acceptance = workload.get("acceptance")
    if not isinstance(acceptance, dict):
        fail("acceptance must be an object")
    if acceptance.get("state") != "pending_parent_gates":
        fail("acceptance must remain pending_parent_gates")
    if acceptance.get("promotion_allowed") is not False:
        fail("pending packet cannot allow promotion")
    if acceptance.get("semantic_activation_allowed") is not False:
        fail("pending packet cannot allow semantic activation")

    if result.get("outcome") != "pending":
        fail("result outcome must remain pending")
    if result.get("acceptance_authority") is not False:
        fail("pending result cannot have acceptance authority")
    if result.get("semantic_activation_disabled") is not True:
        fail("pending result must keep semantics disabled")
    for field in (
        "measured_results",
        "promotion_evidence",
        "fallback_digest",
        "locked_report_digest",
    ):
        if result.get(field) is not None:
            fail(f"pending result must not invent {field}")
    if result.get("parent_gate_receipts") != []:
        fail("pending result must not invent parent gate receipts")
    if set(result.get("blocked_on", [])) != EXPECTED_PARENT_GATES:
        fail("pending result must name every parent gate")


def validate_packet(repository: Path, packet_dir: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    workload = load_object(packet_dir / "workload-v1.json")
    result = load_object(packet_dir / "result-pending.json")
    repository_path(repository, workload.get("schema"), "schema")
    if workload.get("schema_version") != 1:
        fail("unsupported workload schema_version")
    if workload.get("workload_id") != "pr10-locked-semantic-search-v1":
        fail("unexpected workload_id")

    validate_corpus(workload, repository)
    validate_feature_contract(repository)
    validate_production_apis(workload, repository)
    validate_model_integrity(workload, repository)
    validate_async_activation(workload)
    validate_implementation_audit(workload)
    validate_callable_acceptance(workload, repository)
    validate_plan_contract(repository)
    validate_profiles(workload)
    validate_quality_contract(workload)
    validate_resource_and_rollback_contract(workload)
    validate_pending_acceptance(workload, result)
    return workload, result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--strict",
        action="store_true",
        help="require parent-run acceptance; expected to fail while this packet is pending",
    )
    args = parser.parse_args()
    packet_dir = Path(__file__).resolve().parent
    repository = packet_dir.parents[1]
    try:
        workload, result = validate_packet(repository, packet_dir)
    except PacketError as error:
        print(f"invalid PR10 semantic packet: {error}", file=sys.stderr)
        return 2

    if args.strict:
        print(
            "acceptance pending: parent production, locked-quality, resource, "
            "platform, rollback, and aggregate gates have not executed",
            file=sys.stderr,
        )
        return 3

    print(
        "valid PR10 semantic packet; "
        f"state={workload['acceptance']['state']}; "
        f"outcome={result['outcome']}; measured_results=absent"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
