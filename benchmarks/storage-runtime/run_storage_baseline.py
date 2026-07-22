#!/usr/bin/env python3
"""TraceDecay SQLite storage-runtime baseline command-line runner.

The implementation is split by safety and workload ownership. This module is a
small CLI and compatibility facade for existing stdlib consumers.
"""

from __future__ import annotations

import argparse
import sys

from runner_contract import *
from safe_paths import *
from profile_safety import *
from process_execution import *
from workload_model import *
from run_context import *
from evidence_validation import *
from phase_execution import *
from freeze_identity import *
from runner_commands import *
from soak.schemas import ALLOWED_WORKLOAD_IDS

def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="TraceDecay SQLite storage runtime S0 baseline harness"
    )
    sub = parser.add_subparsers(dest="command", required=True)

    freeze = sub.add_parser(
        "freeze", help="capture frozen product/evidence binary and schema identities"
    )
    freeze.add_argument(
        "--product-binary", required=True, help="released TraceDecay product binary to hash"
    )
    freeze.add_argument(
        "--evidence-binary",
        required=True,
        help="storage-runtime-evidence adapter binary to hash",
    )
    freeze.add_argument(
        "--product-commit-sha",
        required=True,
        help="exact source commit identity for the released product binary",
    )
    freeze.add_argument(
        "--product-binary-version-argv",
        nargs="*",
        default=["--version"],
        help="argv appended to --product-binary to capture a version line",
    )
    freeze.add_argument(
        "--schema-manifest",
        required=True,
        help="released schema manifest file to hash (operator-supplied)",
    )
    freeze.add_argument(
        "--workload",
        required=True,
        help="exact workload JSON whose SHA-256 is frozen",
    )
    freeze.add_argument(
        "--corpus",
        required=True,
        help="exact safe corpus tree whose fingerprint is frozen",
    )
    freeze.add_argument(
        "--config",
        required=True,
        help="exact runtime configuration file/tree whose fingerprint is frozen",
    )
    freeze.add_argument("--output", required=True, help="identity artifact path")
    freeze.add_argument(
        "--store-family",
        action="append",
        default=[],
        help="supported store family (repeatable)",
    )
    freeze.add_argument("--notes", default="")
    freeze.set_defaults(func=cmd_freeze)

    run = sub.add_parser("run", help="execute a baseline workload")
    run.add_argument("--workload", required=True, help="workload JSON path")
    run.add_argument(
        "--input",
        required=True,
        help="explicit fixture/copy input directory (never the live profile)",
    )
    run.add_argument(
        "--output",
        required=True,
        help="fresh isolated output directory (must not already exist)",
    )
    run.add_argument(
        "--product-binary", default=None, help="explicit released product binary under test"
    )
    run.add_argument(
        "--evidence-binary", default=None, help="explicit storage evidence adapter binary"
    )
    run.add_argument(
        "--schema-manifest",
        default=None,
        help="schema manifest to bind against --frozen-identity",
    )
    run.add_argument(
        "--config",
        default=None,
        help="runtime config file/tree to bind against --frozen-identity",
    )
    run.add_argument("--frozen-identity", default=None, help="freeze artifact path")
    run.add_argument(
        "--allow-pending",
        action="store_true",
        help="record pending phases as not run instead of failing closed",
    )
    run.add_argument("--only", nargs="*", default=None, help="restrict to phases")
    run.add_argument("--host-label", default=None, help="stable host label")
    run.add_argument(
        "--record-hostname",
        action="store_true",
        help="record the hostname (default redacts it)",
    )
    run.set_defaults(func=cmd_run)

    validate = sub.add_parser("validate", help="validate a result artifact")
    validate.add_argument("--result", required=True)
    validate.set_defaults(func=cmd_validate)

    soak_plan = sub.add_parser(
        "soak-plan", help="write a deterministic soak plan without executing it"
    )
    soak_plan.add_argument("--seed", required=True, type=int)
    soak_plan.add_argument("--duration-seconds", required=True, type=int)
    soak_plan.add_argument("--current-rate", required=True, type=float)
    soak_plan.add_argument("--ten-x-rate", required=True, type=float)
    soak_plan.add_argument("--overload-rate", required=True, type=float)
    soak_plan.add_argument("--crash-count", required=True, type=int)
    soak_plan.add_argument("--restore-rehearsals", required=True, type=int)
    soak_plan.add_argument(
        "--minimum-crash-spacing-seconds", type=float, default=1.0
    )
    soak_plan.add_argument("--sample-interval-seconds", type=float, default=1.0)
    soak_plan.add_argument("--operation-timeout-seconds", type=float, default=120.0)
    soak_plan.add_argument(
        "--workload-id",
        choices=sorted(ALLOWED_WORKLOAD_IDS),
        default="storage-runtime-s11-product-gates-v1",
    )
    soak_plan.add_argument("--output", required=True, help="new plan JSON path")
    soak_plan.set_defaults(func=cmd_soak_plan)

    soak_run = sub.add_parser(
        "soak-run", help="execute a frozen plan through the fixed workload allowlist"
    )
    soak_run.add_argument("--plan", required=True)
    soak_run.add_argument("--product-binary", required=True)
    soak_run.add_argument("--evidence-binary", required=True)
    soak_run.add_argument("--fixture", required=True)
    soak_run.add_argument("--frozen-identity", required=True)
    soak_run.add_argument("--family", choices=("graph", "session"), required=True)
    soak_run.add_argument(
        "--output", required=True, help="fresh runner-owned output directory"
    )
    soak_run.add_argument(
        "--mode",
        choices=("acceptance", "lint"),
        default="acceptance",
        help="acceptance exits nonzero unless the receipt is evidence-eligible",
    )
    soak_run.set_defaults(func=cmd_soak_run)

    soak_evaluate = sub.add_parser(
        "soak-evaluate", help="evaluate explicit soak artifacts without executing work"
    )
    soak_evaluate.add_argument(
        "--baseline",
        action="append",
        required=True,
        help="absolute baseline result path (repeat for each platform)",
    )
    soak_evaluate.add_argument("--plan", required=True, help="explicit soak plan path")
    soak_evaluate.add_argument(
        "--result", required=True, help="explicit soak execution result path"
    )
    soak_evaluate.add_argument("--output", required=True, help="new assessment JSON path")
    soak_evaluate.add_argument(
        "--mode",
        choices=("acceptance", "lint"),
        default="acceptance",
        help="acceptance exits nonzero for not-evidence; lint checks structure only",
    )
    soak_evaluate.set_defaults(func=cmd_soak_evaluate)

    self_test = sub.add_parser(
        "self-test", help="run the checked-in dry-run end to end with assertions"
    )
    self_test.set_defaults(func=cmd_self_test)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return int(args.func(args))
    except RunnerError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
