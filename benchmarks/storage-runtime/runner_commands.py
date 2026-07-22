"""Top-level baseline and soak command orchestration."""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path
from typing import Any

from runner_contract import (
    ConfigError, ExecutionError, LOGICAL_SQLITE_EVIDENCE_SCHEMA, RESULT_ARTIFACT_ID,
    RESULT_SCHEMA_VERSION, SCRUB_ENV_EXACT, SCRUB_ENV_PREFIXES, RunnerError,
    SafetyError,
)
from safe_paths import (
    atomic_write_json_new, atomic_write_new, create_fresh_directory, fingerprint_tree,
    sha256_file, validate_safe_tree,
)
from profile_safety import (
    build_child_env, forbidden_profile_roots, guard_path, normalized_platform_name,
    prepare_output_dir, reject_network_filesystem, require_disjoint_roots,
)
from process_execution import binary_identity, capture_environment, process_tree_capability, require_safe_identifier
from workload_model import effective_phase_pending_reason, load_workload
from run_context import RunContext
from phase_execution import execute_phase_for_family
from evidence_validation import result_contains_absolute_paths, validate_result
from freeze_identity import (
    bind_frozen_binaries,
    bind_frozen_identity,
    frozen_product_commit,
    load_safe_json,
)
from soak.evidence import EvidenceError, assess_evidence, load_baselines
from soak.scheduler import CampaignConfig, ScheduleError, build_campaign
from soak.trends import TrendError, TrendPolicy, evaluate_resource_trends
from soak.executor import execute_soak
from soak.schemas import (
    s6_gate_evidence_eligible,
    validate_plan,
    validate_result as validate_soak_result,
)


def publish_json(
    path: str | Path,
    document: dict,
    role: str,
    *,
    home: Path | None = None,
) -> Path:
    """Publish JSON only after the shared profile/path/filesystem guards."""
    forbidden = forbidden_profile_roots(dict(os.environ), home or Path.home())
    candidate = guard_path(path, role, forbidden)
    reject_network_filesystem(candidate.parent, role)
    return atomic_write_json_new(candidate, document, role, indent=2)


def cmd_soak_plan(args: argparse.Namespace) -> int:
    """Write a deterministic plan; this command never executes the plan."""
    try:
        plan = build_campaign(
            CampaignConfig(
                seed=args.seed,
                duration_seconds=args.duration_seconds,
                rates_per_second={
                    "current": args.current_rate,
                    "ten_x": args.ten_x_rate,
                    "overload": args.overload_rate,
                },
                crash_count=args.crash_count,
                restore_rehearsals=args.restore_rehearsals,
                minimum_crash_spacing_seconds=args.minimum_crash_spacing_seconds,
                sample_interval_seconds=args.sample_interval_seconds,
                operation_timeout_seconds=args.operation_timeout_seconds,
                workload_id=args.workload_id,
            )
        )
    except ScheduleError as exc:
        raise ConfigError(f"invalid soak campaign: {exc}") from exc
    publish_json(args.output, plan, "soak plan")
    return 0


def _trend_policy(document: object) -> TrendPolicy:
    if not isinstance(document, dict):
        raise ConfigError("soak result trend_policy must be an object")
    return TrendPolicy(
        maximum_slope_per_second=document.get("maximum_slope_per_second", {}),
        maximum_end_to_baseline_ratio=document.get(
            "maximum_end_to_baseline_ratio", {}
        ),
        maximum_post_eviction_ratio=document.get("maximum_post_eviction_ratio", {}),
        minimum_samples=document.get("minimum_samples", 3),
        maximum_samples=document.get("maximum_samples", 100_000),
        maximum_cadence_gap_seconds=document.get(
            "maximum_cadence_gap_seconds", 60.0
        ),
    )


def cmd_soak_evaluate(args: argparse.Namespace) -> int:
    """Evaluate only caller-supplied artifacts; never discover or execute work."""
    _, plan = load_safe_json(args.plan, "soak plan")
    _, result = load_safe_json(args.result, "soak result")
    validate_plan(plan)
    validate_soak_result(result)
    samples = result.get("resource_samples")
    post_eviction = result.get("post_eviction")
    receipt = result.get("execution_receipt")
    if not isinstance(samples, list):
        raise ConfigError("soak result resource_samples must be an array")
    if not isinstance(post_eviction, dict):
        raise ConfigError("soak result post_eviction must be an object")
    if not isinstance(receipt, dict):
        raise ConfigError("soak result execution_receipt must be an object")
    try:
        baselines = load_baselines([Path(path) for path in args.baseline])
        trend_policy = _trend_policy(result.get("trend_policy"))
        allowed_gap = 2.0 * float(plan["sample_interval_seconds"])
        if trend_policy.maximum_cadence_gap_seconds > allowed_gap:
            raise ConfigError(
                "soak result cadence policy is weaker than the frozen plan"
            )
        trends = evaluate_resource_trends(
            samples,
            post_eviction,
            trend_policy,
            campaign_duration_seconds=plan["duration_seconds"],
        )
        assessment = assess_evidence(baselines, plan, trends, receipt)
    except (EvidenceError, TrendError, KeyError, TypeError) as exc:
        raise ConfigError(f"invalid soak evaluation input: {exc}") from exc
    publish_json(args.output, assessment, "soak assessment")
    if args.mode == "acceptance" and assessment["status"] != "completed":
        return 2
    return 0


def cmd_soak_run(args: argparse.Namespace) -> int:
    """Execute an immutable plan using only code-owned workload resolvers."""
    home = Path.home()
    forbidden = forbidden_profile_roots(dict(os.environ), home)
    plan_path = guard_path(args.plan, "soak plan", forbidden)
    _, plan = load_safe_json(plan_path, "soak plan")
    validate_plan(plan)
    product_binary = guard_path(args.product_binary, "soak product binary", forbidden)
    evidence_binary = guard_path(args.evidence_binary, "soak evidence binary", forbidden)
    reject_network_filesystem(product_binary, "soak product binary")
    reject_network_filesystem(evidence_binary, "soak evidence binary")
    fixture = validate_safe_tree(
        guard_path(args.fixture, "soak fixture", forbidden), "soak fixture"
    )
    frozen_identity = guard_path(
        args.frozen_identity, "soak frozen identity", forbidden
    )
    _, frozen_document = load_safe_json(frozen_identity, "soak frozen identity")
    frozen_binaries = bind_frozen_binaries(
        frozen_document,
        product_binary_path=product_binary,
        evidence_binary_path=evidence_binary,
    )
    product_commit_sha = frozen_product_commit(frozen_document)
    output_root = prepare_output_dir(args.output, forbidden)
    reject_network_filesystem(fixture, "soak fixture")
    reject_network_filesystem(output_root, "soak output")
    require_disjoint_roots(fixture, output_root)
    run_root = create_fresh_directory(output_root / "executor", "soak executor")
    result = asyncio.run(
        execute_soak(
            plan,
            product_binary=product_binary,
            evidence_binary=evidence_binary,
            frozen_binary_identities=frozen_binaries,
            product_commit_sha=product_commit_sha,
            fixture=fixture,
            frozen_identity=frozen_identity,
            family=args.family,
            run_root=run_root,
            forbidden=forbidden,
        )
    )
    validate_safe_tree(output_root, "soak output")
    atomic_write_json_new(
        output_root / "storage-runtime-soak-result.json",
        result,
        "soak result",
        indent=2,
    )
    receipt = result["execution_receipt"]
    sustained_complete = all(
        item["offered"] == item["terminal"]
        and item["offered"] == item["admitted"] + item["shed_runner_in_flight"]
        and item["admitted"]
        == item["completed"] + item["failed"] + item["shed_command_saturation"]
        and item["failed"] == 0
        for item in receipt["sustained"]
    )
    product_gates_complete = True
    if plan["workload_id"] == "storage-runtime-s11-product-gates-v1":
        product_gates = receipt.get("product_gate_evidence") or []
        product_commits = {
            gate.get("product_commit_sha")
            for gate in product_gates
            if isinstance(gate, dict)
        }
        product_gates_complete = (
            len(product_gates) == 3
            and all(s6_gate_evidence_eligible(gate) for gate in product_gates)
            and len(product_commits) == 1
            and receipt.get("product_commit_sha") in product_commits
            and all(
                gate.get("product_binary_sha256")
                == receipt.get("product_binary_sha256")
                and gate.get("evidence_binary_sha256")
                == receipt.get("evidence_binary_sha256")
                for gate in product_gates
            )
            and receipt.get("product_binary_sha256")
            != receipt.get("evidence_binary_sha256")
        )
    evidence_eligible = (
        receipt["status"] == "completed"
        and receipt["product_adapter_validated"] is True
        and bool(receipt["logical_evidence"])
        and product_gates_complete
        and sustained_complete
        and receipt["crash_count_completed"] == plan["crash_count"]
        and receipt["crash_recovery_count"] == plan["crash_count"]
        and receipt["restore_rehearsal_count"] == plan["restore_rehearsal_count"]
        and receipt["restore_verified_count"] == plan["restore_rehearsal_count"]
    )
    if args.mode == "acceptance" and not evidence_eligible:
        return 2
    return 0 if receipt["status"] == "completed" else 2

def cmd_run(args: argparse.Namespace) -> int:
    home = Path.home()
    forbidden = forbidden_profile_roots(dict(os.environ), home)
    workload_path = guard_path(args.workload, "workload", forbidden)
    workload = load_workload(workload_path)
    if args.host_label is not None:
        require_safe_identifier(args.host_label, "host label")
    tree_capability = process_tree_capability()
    if tree_capability["state"] != "supported_best_effort":
        raise SafetyError(
            "workload execution requires verifiable stdlib process-group cleanup; "
            f"this platform reports {tree_capability['state']}"
        )
    input_root = guard_path(args.input, "input", forbidden)
    input_root = validate_safe_tree(input_root, "input")
    output_candidate = guard_path(args.output, "output", forbidden)
    require_disjoint_roots(input_root, output_candidate)
    input_filesystem = reject_network_filesystem(input_root, "input")
    output_filesystem = reject_network_filesystem(output_candidate.parent, "output")
    reject_network_filesystem(workload_path, "workload")

    current_platform = normalized_platform_name()
    platform_config = workload.get("platforms") or {}
    required_platforms = list(platform_config.get("required") or [current_platform])
    if current_platform not in required_platforms:
        raise ConfigError(
            f"workload does not admit normalized platform {current_platform!r}; "
            f"required={required_platforms}"
        )

    only_requested = args.only is not None
    only = set(args.only or [])
    unknown = only - {phase["name"] for phase in workload["phases"]}
    if unknown:
        raise ConfigError(f"--only references unknown phases {sorted(unknown)}")
    phases = [
        phase for phase in workload["phases"] if not only or phase["name"] in only
    ]
    # Pending product steps fail before any output root is created unless the
    # operator explicitly requests a not-evidence record.
    if not args.allow_pending:
        for phase in phases:
            reason = effective_phase_pending_reason(workload, phase)
            if reason is not None:
                raise ConfigError(
                    f"phase {phase['name']!r} is pending ({reason}); refusing to execute. "
                    "Re-run with --allow-pending to record it as not-evidence."
                )

    safety_cfg = workload.get("safety", {})
    # Validate declarations now, before creating any output.  Protected roots
    # are rejected; each run gets fixed runner-owned locations instead.
    build_child_env(
        dict(os.environ),
        dict(safety_cfg.get("env") or {}),
        list(safety_cfg.get("env_path_keys") or []),
        forbidden,
    )

    input_fingerprint = fingerprint_tree(input_root, "input corpus")
    product_binary = args.product_binary or workload.get("product_binary")
    evidence_binary = args.evidence_binary or workload.get("evidence_binary")
    if product_binary:
        product_binary_path = guard_path(product_binary, "product binary", forbidden)
        reject_network_filesystem(product_binary_path, "product binary")
        binary_identity(product_binary_path)
        product_binary = str(product_binary_path)
    if evidence_binary:
        evidence_binary_path = guard_path(evidence_binary, "evidence binary", forbidden)
        reject_network_filesystem(evidence_binary_path, "evidence binary")
        binary_identity(evidence_binary_path)
        evidence_binary = str(evidence_binary_path)
    frozen_ref = workload.get("frozen_identity", {})
    frozen_identity: dict[str, Any]
    identity_binding: dict[str, Any]
    config_source: Path | None = None
    bound_identity_document: dict[str, Any] | None = None
    bound_schema_manifest: Path | None = None
    if args.frozen_identity:
        identity_path = guard_path(args.frozen_identity, "frozen identity", forbidden)
        reject_network_filesystem(identity_path, "frozen identity")
        identity_path, identity = load_safe_json(identity_path, "frozen identity")
        if (
            not product_binary
            or not evidence_binary
            or not args.schema_manifest
            or not args.config
        ):
            raise ConfigError(
                "a frozen identity requires --product-binary, --evidence-binary, "
                "--schema-manifest, and --config to bind all tested artifacts"
            )
        schema_manifest_path = guard_path(args.schema_manifest, "schema manifest", forbidden)
        config_path = guard_path(args.config, "config", forbidden)
        reject_network_filesystem(schema_manifest_path, "schema manifest")
        reject_network_filesystem(config_path, "config")
        identity_binding = bind_frozen_identity(
            identity,
            product_binary_path=guard_path(
                product_binary, "product binary", forbidden
            ),
            evidence_binary_path=guard_path(
                evidence_binary, "evidence binary", forbidden
            ),
            schema_manifest_path=schema_manifest_path,
            workload_path=workload_path,
            corpus_root=input_root,
            config_path=config_path,
        )
        config_source = config_path
        bound_identity_document = identity
        bound_schema_manifest = schema_manifest_path
        frozen_identity = {
            "status": "supplied",
            "basename": identity_path.name,
            "sha256": sha256_file(identity_path, "frozen identity"),
            "schema_version": identity["schema_version"],
        }
    elif frozen_ref.get("required_for_evidence"):
        raise ConfigError(
            "workload requires a frozen identity artifact; supply --frozen-identity "
            "with --product-binary, --evidence-binary, --schema-manifest, and --config"
        )
    else:
        frozen_identity = {"status": "not_supplied"}
        identity_binding = {
            "status": "not_bound",
            "reason": "no frozen identity was supplied; this result is not evidence",
        }

    output_root = prepare_output_dir(args.output, forbidden)

    ctx = RunContext(
        workload=workload,
        input_root=input_root,
        output_root=output_root,
        base_env=dict(os.environ),
        forbidden=forbidden,
        timeout_default=float(workload.get("defaults", {}).get("timeout_seconds", 60.0)),
        product_binary=product_binary,
        evidence_binary=evidence_binary,
        config_source=config_source,
        bound_corpus=(
            identity_binding.get("components", {}).get("corpus")
            if identity_binding["status"] == "bound"
            else None
        ),
        bound_product_binary=(
            identity_binding.get("components", {}).get("product_binary")
            if identity_binding["status"] == "bound"
            else None
        ),
        bound_evidence_binary=(
            identity_binding.get("components", {}).get("evidence_binary")
            if identity_binding["status"] == "bound"
            else None
        ),
        bound_config=(
            identity_binding.get("components", {}).get("config")
            if identity_binding["status"] == "bound"
            else None
        ),
    )

    execution_failures: list[dict[str, str]] = []
    for phase in phases:
        for family in phase["families"]:
            try:
                execute_phase_for_family(ctx, phase, family, args.allow_pending)
            except ExecutionError as exc:
                # Preserve a terminal, explicitly non-evidence record without
                # serializing potentially sensitive child stdout/stderr.
                ctx.runs.append(
                    {
                        "phase": phase["name"],
                        "family": family,
                        "kind": phase["kind"],
                        "status": "failed",
                        "failure_class": type(exc).__name__,
                    }
                )
                execution_failures.append(
                    {"phase": phase["name"], "family": family, "class": type(exc).__name__}
                )

    if bound_identity_document is not None:
        # Re-read every bound artifact before publishing.  This detects an
        # external mutation after preflight and ensures the final result cannot
        # claim a freeze identity that differs from what its child processes saw.
        if (
            config_source is None
            or bound_schema_manifest is None
            or not product_binary
            or not evidence_binary
        ):
            raise SafetyError("bound identity state was lost before publication")
        identity_binding = bind_frozen_identity(
            bound_identity_document,
            product_binary_path=product_binary,
            evidence_binary_path=evidence_binary,
            schema_manifest_path=bound_schema_manifest,
            workload_path=workload_path,
            corpus_root=input_root,
            config_path=config_source,
        )

    environment = capture_environment(
        workload,
        args.host_label,
        bool(args.record_hostname),
        create_fresh_directory(output_root / "environment-probe", "environment probe"),
        forbidden,
    )
    # Commands are allowed only to mutate their own copy/sandbox.  Scan the
    # entire runner-owned output before publication so links, special files,
    # and hardlinks created by a child cannot become benchmark artifacts.
    validate_safe_tree(output_root, "runner output")

    limitations = list(workload.get("limitations") or [])
    not_evidence_reasons: list[str] = []
    pending = any(run.get("status") == "pending" for run in ctx.runs)
    if pending:
        not_evidence_reasons.append("one or more phases are pending and produced no measurements")
    if only_requested:
        not_evidence_reasons.append("--only was supplied; selected-phase output is partial")
    if execution_failures:
        not_evidence_reasons.append("one or more phase/family executions failed")
    if identity_binding["status"] != "bound":
        not_evidence_reasons.append("tested artifacts are not bound to a frozen identity")
    if not workload["evidence_eligible"]:
        not_evidence_reasons.append("workload is explicitly ineligible for product evidence")
    if input_filesystem["state"] != "local" or output_filesystem["state"] != "local":
        not_evidence_reasons.append(
            "input/output filesystem locality could not be verified"
        )
    if not_evidence_reasons:
        limitations.extend(not_evidence_reasons)
    scope_mode = "partial" if (only_requested or pending or execution_failures) else "full"
    evidence_state = "not_evidence" if not_evidence_reasons else "evidence"

    result = {
        "artifact_id": RESULT_ARTIFACT_ID,
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": "completed" if evidence_state == "evidence" else "not_evidence",
        "evidence_status": {"state": evidence_state, "reasons": not_evidence_reasons},
        "execution_scope": {
            "mode": scope_mode,
            "only_requested": only_requested,
            "selected_phase_ids": [phase["name"] for phase in phases],
        },
        "workload": {
            "id": workload["workload_id"],
            "basename": workload_path.name,
            "sha256": sha256_file(workload_path, "workload"),
            "evidence_eligible": workload["evidence_eligible"],
        },
        "frozen_identity": frozen_identity,
        "identity_binding": identity_binding,
        "environment": environment,
        "platform": {
            "current": current_platform,
            "required": required_platforms,
            "configured_status": platform_config.get("status", {}),
            "enforcement": "current platform is a normalized required platform",
        },
        "process_tree_control": tree_capability,
        "safety": {
            "live_profile_guard": "enforced",
            "forbidden_roots_checked": [label for label, _ in forbidden],
            "child_env_scrubbed_prefixes": list(SCRUB_ENV_PREFIXES),
            "child_env_scrubbed_exact": list(SCRUB_ENV_EXACT),
            "recursive_lstat_tree_guard": "enforced",
            "unsafe_hardlinks": "rejected",
            "input_output_disjoint": "enforced",
            "fresh_runner_owned_store_copy_per_run": "enforced",
            "output_publication": "create_new_no_follow_atomic_link",
            "input_filesystem": input_filesystem,
            "output_filesystem": output_filesystem,
            "input_fingerprint_basis": "relative paths and SHA-256 only",
        },
        "logical_evidence_schema": LOGICAL_SQLITE_EVIDENCE_SCHEMA,
        "input_fingerprint": {
            "file_count": input_fingerprint["file_count"],
            "aggregate_sha256": input_fingerprint["aggregate_sha256"],
        },
        "runs": ctx.runs,
        "limitations": limitations,
    }

    problems = validate_result(result)
    absolute_hits = result_contains_absolute_paths(result)
    if absolute_hits:
        raise SafetyError(
            f"absolute paths leaked into result at {absolute_hits}; refusing publication"
        )
    if problems:
        result["status"] = "failed_validation"
        result["evidence_status"] = {
            "state": "not_evidence",
            "reasons": [*not_evidence_reasons, "result validation failed"],
        }
    if problems:
        result["validation_problems"] = problems

    result_path = output_root / "storage-runtime-baseline-result.json"
    atomic_write_new(
        result_path, json.dumps(result, indent=2, sort_keys=True) + "\n", "baseline result"
    )
    print(f"[s0] result written to {result_path}", file=sys.stderr)
    if problems:
        for problem in problems:
            print(f"[s0] validation problem: {problem}", file=sys.stderr)
        return 2
    return 2 if execution_failures else 0


def cmd_validate(args: argparse.Namespace) -> int:
    result_path, result = load_safe_json(args.result, "result artifact")
    problems = validate_result(result)
    problems.extend(
        f"absolute path leaked at {hit}"
        for hit in result_contains_absolute_paths(result)
    )
    if problems:
        for problem in problems:
            print(f"invalid: {problem}", file=sys.stderr)
        return 2
    print(f"valid: {result_path}")
    return 0


def cmd_self_test(args: argparse.Namespace) -> int:
    del args
    here = Path(__file__).resolve().parent
    workload_path = here / "workload-dry-run.json"
    fixture_src = here / "fixtures" / "dry-run-input"
    if not workload_path.is_file() or not fixture_src.is_dir():
        raise ConfigError("dry-run workload or fixture directory is missing")

    failures: list[str] = []

    def check(condition: bool, message: str) -> None:
        if not condition:
            failures.append(message)
        print(f"[self-test] {'PASS' if condition else 'FAIL'}: {message}")

    def run_for_self_test(workload: Path, input_root: Path, output_root: Path) -> int:
        run_args = argparse.Namespace(
            workload=str(workload),
            input=str(input_root),
            output=str(output_root),
            product_binary=None,
            evidence_binary=None,
            schema_manifest=None,
            config=None,
            frozen_identity=None,
            allow_pending=False,
            only=None,
            host_label=None,
            record_hostname=False,
        )
        try:
            return cmd_run(run_args)
        except RunnerError as exc:
            print(f"error: {exc}", file=sys.stderr)
            return 2

    with tempfile.TemporaryDirectory(prefix="tracedecay-s0-selftest-") as tmp:
        tmp_root = Path(tmp)
        input_dir = tmp_root / "input"
        shutil.copytree(fixture_src, input_dir)
        output_dir = tmp_root / "output"

        # 1. Guard refuses a live-profile aliased output directory.
        fake_live = tmp_root / "fake-home" / ".tracedecay"
        fake_live.mkdir(parents=True)
        env = dict(os.environ)
        env["TRACEDECAY_DATA_DIR"] = str(fake_live)
        forbidden = forbidden_profile_roots(env, tmp_root / "fake-home")
        refused = False
        try:
            prepare_output_dir(fake_live / "nested", forbidden)
        except SafetyError:
            refused = True
        check(refused, "guard refuses output inside a live profile location")

        alias = tmp_root / "alias-to-live"
        try:
            alias.symlink_to(fake_live)
            refused = False
            try:
                prepare_output_dir(alias, forbidden)
            except SafetyError:
                refused = True
            check(refused, "guard refuses a symlink alias of a live profile location")
        except OSError:
            check(True, "symlink unsupported on this platform; alias check skipped")

        # 2. Child environment never inherits TraceDecay discovery variables.
        env["TRACEDECAY_GLOBAL_DB"] = str(fake_live / "global.db")
        child_env = build_child_env(env, {}, [], forbidden)
        check(
            not any(key.startswith("TRACEDECAY_") for key in child_env),
            "child environment strips TRACEDECAY_* variables",
        )

        # 3. Full dry-run execution end to end.
        rc = run_for_self_test(workload_path, input_dir, output_dir)
        check(rc == 0, f"dry-run workload executes cleanly (rc={rc})")

        result_path = output_dir / "storage-runtime-baseline-result.json"
        check(result_path.is_file(), "result artifact was written")
        if result_path.is_file():
            result = json.loads(result_path.read_text())
            problems = validate_result(result)
            check(not problems, f"result validates ({problems})")
            leaks = result_contains_absolute_paths(result)
            check(not leaks, f"result contains no absolute paths ({leaks})")
            check(
                result["status"] == "not_evidence"
                and result["evidence_status"]["state"] == "not_evidence",
                "dry-run output is explicitly not-evidence",
            )
            phases_run = {run["phase"] for run in result["runs"]}
            expected = {
                "current",
                "ten_x",
                "overload",
                "crash",
                "recovery",
                "fts",
                "backup_restore",
                "aa_noise",
            }
            check(
                expected <= phases_run,
                f"all dry-run phases executed (missing {sorted(expected - phases_run)})",
            )
            aa_runs = [
                run
                for run in result["runs"]
                if run["phase"] == "aa_noise" and run.get("aa")
            ]
            check(bool(aa_runs), "A/A noise-floor analysis recorded")
            if aa_runs:
                floor = aa_runs[0]["aa"]["noise_floor"]["p50_response_ns"]
                check(
                    floor["regression_margin_relative"] is not None,
                    "A/A regression margin computed",
                )

        # 4. Pending product steps fail closed without --allow-pending.
        pending_workload = tmp_root / "pending-workload.json"
        pending_doc = json.loads(workload_path.read_text())
        pending_doc["phases"][0]["work"]["argv"] = None
        pending_workload.write_text(json.dumps(pending_doc))
        output_dir2 = tmp_root / "output2"
        rc_pending = run_for_self_test(pending_workload, input_dir, output_dir2)
        check(
            rc_pending == 2 and not (output_dir2 / "storage-runtime-baseline-result.json").exists(),
            "pending steps fail closed without --allow-pending",
        )

    if failures:
        print(f"[self-test] {len(failures)} failure(s)", file=sys.stderr)
        return 1
    print("[self-test] all checks passed", file=sys.stderr)
    return 0
