#!/usr/bin/env bash
set -euo pipefail

python3 - \
  .github/workflows/ci.yml \
  .github/workflows/release.yml \
  .github/workflows/release-beta.yml \
  .github/workflows/release-plz.yml \
  .github/workflows/release-pr-integrity.yml \
  .github/workflows/plugin-validation.yml <<'PY'
import fnmatch
import os
import pathlib
import re
import sys

ci, stable, beta, release_plz, release_integrity, plugin = [
    open(path, encoding="utf-8").read() for path in sys.argv[1:]
]


def job_block(workflow: str, job: str) -> str:
    marker = f"  {job}:"
    if marker not in workflow:
        raise SystemExit(f"workflow is missing expected job {job!r}")
    return re.split(r"\n  (?=\S)", workflow.split(marker, 1)[1], maxsplit=1)[0]


for name, workflow in [
    ("CI", ci),
    ("stable release", stable),
    ("beta release", beta),
    ("release-plz", release_plz),
    ("plugin validation", plugin),
]:
    for required in [
        "dashboard-assets:",
        "npm run build",
        "scripts/check-dashboard-bundle.py",
        "actions/upload-artifact@",
        "name: dashboard-app-dist",
        "actions/download-artifact@",
        "path: dashboard/app-dist",
        'TRACEDECAY_SKIP_DASHBOARD_BUILD: "1"',
    ]:
        if required not in workflow:
            raise SystemExit(f"{name} workflow is missing dashboard artifact contract {required!r}")

for job in [
    "test",
    "windows-build",
    "windows-pr12-pr13-packets",
    "release-compatibility",
    "clippy",
    "dashboard",
    "hermes-integration",
]:
    block = job_block(ci, job)
    if "dashboard-assets" not in block:
        raise SystemExit(f"CI Rust job {job!r} must wait for dashboard-assets")
    if "actions/download-artifact@" not in block:
        raise SystemExit(f"CI Rust job {job!r} must download dashboard-app-dist")

dashboard_job = job_block(ci, "dashboard")
for required in [
    "npm run typecheck",
    "npm run contracts:check",
    "npm test",
    "npm run boundary:check",
    "cargo nextest run --all-features --test dashboard_api_test",
    "--no-tests=fail",
]:
    if required not in dashboard_job:
        raise SystemExit(
            f"CI dashboard integration job must preserve frontend check {required!r}"
        )

# The boundary gate needs ast-grep on PATH, and the install must come after
# setup-node or the global bin can belong to a different Node toolchain.
if dashboard_job.index("actions/setup-node@") > dashboard_job.index("Install ast-grep"):
    raise SystemExit("CI dashboard job must install ast-grep after actions/setup-node")
if dashboard_job.index("Install ast-grep") > dashboard_job.index("npm run boundary:check"):
    raise SystemExit("CI dashboard job must install ast-grep before the boundary gate")

# Plan 11 makes WCAG 2.2 AA and the payload ceilings acceptance criteria, so
# they are gates rather than scripts a developer may remember to run. The budget
# check belongs to the artifact build: it measures the bytes being uploaded.
assets_job = job_block(ci, "dashboard-assets")
if "scripts/check-dashboard-budget.mjs" not in assets_job:
    raise SystemExit(
        "CI dashboard-assets job must preserve the payload budget gate "
        "'scripts/check-dashboard-budget.mjs'"
    )

# The accessibility gates are their own job. Every Rust job declares
# `needs: dashboard-assets`, so an axe failure inside dashboard-assets skipped
# the entire Rust matrix and destroyed the signal about whether Rust passed.
# Each harness also runs its own full rsbuild build, which alone blew that
# job's timeout budget.
accessibility_job = job_block(ci, "dashboard-accessibility")
for required in [
    "playwright install",
    "npm run axe:audit",
    "npm run axe:explorer",
    "needs: dashboard-assets",
    "actions/download-artifact@",
    "path: dashboard/app-dist",
]:
    if required not in accessibility_job:
        raise SystemExit(
            f"CI dashboard-accessibility job must preserve {required!r}"
        )

# Keeping the gates out of dashboard-assets is the entire point of the split;
# a well-meaning "run them where the bundle is" edit would undo it silently.
for forbidden in ["playwright install", "npm run axe:"]:
    if forbidden in assets_job:
        raise SystemExit(
            f"CI dashboard-assets job must not run {forbidden!r}: the Rust "
            "matrix needs it, so an accessibility failure would skip every "
            "Rust job. Keep it in dashboard-accessibility."
        )

# Nothing may depend on the accessibility gate, or its failure would skip
# whatever does and reintroduce the blast radius this split removed.
jobs_section = ci.split("\njobs:\n", 1)[1]
for job_name in re.findall(r"(?m)^  ([A-Za-z0-9_-]+):$", jobs_section):
    if job_name == "dashboard-accessibility":
        continue
    if "dashboard-accessibility" in job_block(ci, job_name):
        raise SystemExit(
            f"CI job {job_name!r} must not depend on dashboard-accessibility"
        )

for name, workflow, jobs in [
    ("stable release", stable, ["build", "package-workspace"]),
    ("beta release", beta, ["build", "package-workspace"]),
    ("release-plz", release_plz, ["release-plz-release", "release-plz-pr"]),
    ("plugin validation", plugin, ["mcp-conformance-smoke"]),
]:
    for job in jobs:
        block = job_block(workflow, job)
        if "dashboard-assets" not in block:
            raise SystemExit(f"{name} job {job!r} must wait for dashboard-assets")
        if "actions/download-artifact@" not in block:
            raise SystemExit(f"{name} job {job!r} must download dashboard-app-dist")

if "cargo " in release_integrity or "npm run build" in release_integrity:
    raise SystemExit("release PR integrity must remain a read-only path guard")

# --------------------------------------------------------------------------
# nextest filter clauses must name tests that exist.
#
# `windows-pr8-temporal-durable` is the ONLY job that omits
# TRACEDECAY_SQLITE_UNSAFE_FAST, so it is the only Windows coverage of DELETE +
# FULL pragmas. Its filter carried `binary(=session_suite) & test(/^lcm_schema::/)`
# after `mod lcm_schema;` was removed from tests/session_suite/main.rs. nextest
# does not complain about one empty clause in a union, so the job ran a
# nonempty set, went green, and silently covered 30 fewer tests.
#
# --no-tests=fail (asserted below) only catches a filter that matches nothing at
# ALL, so it would not have caught this. These checks are the per-clause
# guarantee: every `test(/^module::/)` clause is resolved back to the module that
# has to exist for it to match anything.
# --------------------------------------------------------------------------
durable_job = job_block(ci, "windows-pr8-temporal-durable")
filter_match = re.search(r"\$filter = '([^']*)'", durable_job)
if filter_match is None:
    raise SystemExit("windows-pr8-temporal-durable must define a $filter expression")
durable_filter = filter_match.group(1)

durable_commands = "\n".join(
    line for line in durable_job.splitlines() if not line.lstrip().startswith("#")
)
if "--no-tests=fail" not in durable_commands:
    raise SystemExit(
        "windows-pr8-temporal-durable must pass --no-tests=fail so a filter that "
        "matches nothing at all fails instead of reporting success"
    )

# Split the union into per-binary segments so each test() prefix is checked
# against the binary it is scoped to.
binary_segments: dict[str, str] = {}
binary_positions = [
    (match.group(1), match.start())
    for match in re.finditer(r"binary\(=([A-Za-z0-9_]+)\)", durable_filter)
]
for index, (binary_name, start) in enumerate(binary_positions):
    end = (
        binary_positions[index + 1][1]
        if index + 1 < len(binary_positions)
        else len(durable_filter)
    )
    binary_segments[binary_name] = durable_filter[start:end]

# Integration-test binaries: `tests/<binary>/main.rs` must declare the module.
for binary_name in ["session_suite", "storage_suite"]:
    segment = binary_segments.get(binary_name)
    if segment is None:
        raise SystemExit(
            f"durable filter must still cover binary(={binary_name})"
        )
    modules = re.findall(r"test\(/\^([A-Za-z0-9_]+)::", segment)
    if not modules:
        raise SystemExit(f"durable filter names no modules for {binary_name}")
    main_rs = pathlib.Path("tests") / binary_name / "main.rs"
    declared = re.findall(r"(?m)^\s*mod ([A-Za-z0-9_]+);", main_rs.read_text(encoding="utf-8"))
    for module in modules:
        if module not in declared:
            raise SystemExit(
                f"durable filter clause 'binary(={binary_name}) & "
                f"test(/^{module}::/)' matches nothing: {main_rs.as_posix()} "
                f"does not declare 'mod {module};'"
            )

# The LCM schema tests live in the library binary via a #[path] injection, not
# in session_suite. If that injection moves or is renamed the filter prefix
# stops matching, so pin the injection and the prefix to each other.
schema_rs = pathlib.Path("src/global_db/session_temporal/schema.rs")
injection = re.search(
    r'#\[path = "\.\./\.\./\.\./tests/session_suite/lcm_schema/mod\.rs"\]\s*\n\s*mod ([A-Za-z0-9_]+);',
    schema_rs.read_text(encoding="utf-8"),
)
if injection is None:
    raise SystemExit(
        f"{schema_rs.as_posix()} must keep the #[path] injection of "
        "tests/session_suite/lcm_schema/mod.rs; the Windows durable filter "
        "reaches those tests through it"
    )
lcm_prefix = f"global_db::session_temporal::schema::{injection.group(1)}::"
tracedecay_segment = binary_segments.get("tracedecay", "")
if f"test(/^{lcm_prefix}/)" not in tracedecay_segment:
    raise SystemExit(
        "durable filter must cover the LCM schema tests as "
        f"'binary(=tracedecay) & test(/^{lcm_prefix}/)' - that is where the "
        "#[path] injection puts them"
    )

# --------------------------------------------------------------------------
# libtest-filtered gates must prove a test ran.
#
# `cargo test <name> -- --exact` exits 0 when the filter matches NOTHING, so
# every name-filtered `cargo test` in CI is one rename away from becoming a
# silent no-op that still reports success. scripts/require-exact-test.sh
# asserts the count libtest prints.
# --------------------------------------------------------------------------
guard = pathlib.Path("scripts/require-exact-test.sh")
if not guard.is_file():
    raise SystemExit(f"{guard.as_posix()} must exist to guard name-filtered gates")
if not os.access(guard, os.X_OK):
    raise SystemExit(f"{guard.as_posix()} must be executable")

# Both the Linux/macOS `test` job and the Windows packets job carry these
# gates and the platform-lifecycle receipt, so both are held to the same rule.
gate_jobs = {
    "test": 3,  # lite grammar, platform lifecycle, observation crash harness
    "windows-pr12-pr13-packets": 2,  # lite grammar, platform lifecycle
}
for job_name, guarded_gates in gate_jobs.items():
    block = job_block(ci, job_name)
    # Drop comment lines (YAML and shell alike - neither runs anything), then
    # fold block scalars and shell line continuations so a guarded invocation
    # reads as one command; otherwise the wrapped `cargo test ...` continuation
    # line looks bare.
    commands = "\n".join(
        line for line in block.splitlines() if not line.lstrip().startswith("#")
    )
    folded = re.sub(r"\s+", " ", commands.replace("\\\n", " "))
    for match in re.finditer(r"\bcargo test ", folded):
        preceding = folded[max(0, match.start() - 40) : match.start()]
        if "require-exact-test.sh" not in preceding:
            raise SystemExit(
                f"{job_name} must run name-filtered cargo tests through "
                "scripts/require-exact-test.sh, not bare: "
                f"{folded[match.start() : match.start() + 90]!r}"
            )
    if block.count("scripts/require-exact-test.sh") < guarded_gates:
        raise SystemExit(
            f"{job_name} must guard all {guarded_gates} name-filtered gates "
            "with scripts/require-exact-test.sh"
        )

    # The platform-lifecycle receipt must be written by the step that ran the
    # test and required by the step that reports it, never created after the
    # fact by a step that cannot know whether the test ran.
    if ": > pr12-pr13-os-evidence" in block or ': > "pr12-pr13-os-evidence' in block:
        raise SystemExit(
            f"{job_name} must not create platform_lifecycle.passed "
            "unconditionally: only the step that ran the default-feature "
            "lifecycle test can attest to it"
        )
    if not re.search(r"test -s \"?pr12-pr13-os-evidence/\S*platform_lifecycle\.passed", block):
        raise SystemExit(
            f"{job_name} must REQUIRE platform_lifecycle.passed before passing "
            "--gate-passed platform_*_lifecycle"
        )
    lifecycle_step = block.split("- name: PR13 default-feature platform lifecycle", 1)
    if len(lifecycle_step) != 2:
        raise SystemExit(f"{job_name} must keep the PR13 platform lifecycle gate")
    lifecycle_body = lifecycle_step[1].split("- name: ", 1)[0]
    if "platform_lifecycle.passed" not in lifecycle_body:
        raise SystemExit(
            f"{job_name}'s PR13 default-feature platform lifecycle step must "
            "write platform_lifecycle.passed itself"
        )

    # pr13_lite_grammar_contract is feature-scoped: the --all-features junit
    # shares its test name and cannot witness the lite build, so the validator
    # refuses to close it from junit and --gate-passed is the only route. That
    # flag therefore has to rest on a receipt from the step that ran the lite
    # build, exactly like the platform lifecycle one.
    lite_step = block.split("- name: PR13 lite grammar gate", 1)
    if len(lite_step) != 2:
        raise SystemExit(f"{job_name} must keep the PR13 lite grammar gate")
    lite_body = lite_step[1].split("- name: ", 1)[0]
    if "lite_grammar.passed" not in lite_body:
        raise SystemExit(
            f"{job_name}'s PR13 lite grammar gate must write lite_grammar.passed "
            "itself; --gate-passed pr13_lite_grammar_contract rests on it"
        )
    # Check the cargo invocation itself. Both the step comment and the receipt
    # text name the flag, so scanning the whole step body would pass even after
    # the command silently changed to --all-features.
    lite_commands = "\n".join(
        line for line in lite_body.splitlines() if not line.lstrip().startswith("#")
    )
    lite_folded = re.sub(r"\s+", " ", lite_commands.replace("\\\n", " "))
    invocation = re.search(r"cargo test (.*?) --test ", lite_folded)
    if invocation is None:
        raise SystemExit(
            f"{job_name}'s PR13 lite grammar gate must run a `cargo test --test` "
            "invocation"
        )
    if "--no-default-features" not in invocation.group(1):
        raise SystemExit(
            f"{job_name}'s PR13 lite grammar gate must keep --no-default-features "
            f"(found: cargo test {invocation.group(1)}). Without it the gate stops "
            "being feature-scoped and becomes closable by the all-features junit "
            "again, which is the hole this receipt exists to close"
        )
    if not re.search(r"test -s \"?pr12-pr13-os-evidence/\S*lite_grammar\.passed", block):
        raise SystemExit(
            f"{job_name} must REQUIRE lite_grammar.passed before passing "
            "--gate-passed pr13_lite_grammar_contract"
        )

# The aggregate job runs no cargo at all, so every --gate-passed it asserts has
# to rest on a receipt downloaded from the job that did the work.
aggregate_job = job_block(ci, "pr12-pr13-platform-aggregate")
if "--gate-passed pr13_lite_grammar_contract" in aggregate_job and not re.search(
    r"test -s \"pr12-pr13-os-evidence/\$\{os_name\}/lite_grammar\.passed\"", aggregate_job
):
    raise SystemExit(
        "pr12-pr13-platform-aggregate asserts --gate-passed "
        "pr13_lite_grammar_contract but never ran the lite build, so it must "
        "require each OS lite_grammar.passed receipt"
    )

# --------------------------------------------------------------------------
# Raw nextest junit must survive a failing test step.
#
# The OS-tagged pr12-pr13 upload runs only after the strict validators, so a
# red `Run tests` step ended the job before anything wrote the durations and
# failure detail anywhere durable. Every Linux/macOS run must therefore keep an
# unconditional raw copy, uploaded before the first gate that can abort.
# --------------------------------------------------------------------------


def steps_of(block: str) -> list[str]:
    return [step for step in re.split(r"\n(?=      - )", block) if step.strip()]


def step_title(step: str) -> str:
    match = re.match(r"\s*- (?:name: (.+)|uses: (.+))", step.lstrip("\n"))
    if match is None:
        return step.strip().splitlines()[0]
    return (match.group(1) or match.group(2)).strip()


def with_mapping(step: str) -> str:
    return step.split("with:", 1)[1] if "with:" in step else ""


test_steps = steps_of(job_block(ci, "test"))
raw_junit_indexes = [
    index
    for index, step in enumerate(test_steps)
    if "actions/upload-artifact@" in step
    and "path: target/nextest/ci/junit.xml" in step
]
if len(raw_junit_indexes) != 1:
    raise SystemExit(
        "CI test job must upload the raw nextest junit "
        "('path: target/nextest/ci/junit.xml') exactly once; found "
        f"{len(raw_junit_indexes)}"
    )
raw_junit_index = raw_junit_indexes[0]
raw_junit_step = test_steps[raw_junit_index]

if not re.search(r"(?m)^\s+if: always\(\)\s*$", raw_junit_step):
    raise SystemExit(
        "CI test job's raw nextest junit upload must be `if: always()`, or a "
        "failing test step keeps taking the junit down with it"
    )
if "retention-days: 7" not in with_mapping(raw_junit_step):
    raise SystemExit(
        "CI test job's raw nextest junit upload must set retention-days: 7"
    )
# A missing junit means the compile or the runner died. That is already a job
# failure; the upload must not add a second, misleading one, and must not
# swallow the fact that there was nothing to keep either.
if "if-no-files-found: warn" not in with_mapping(raw_junit_step):
    raise SystemExit(
        "CI test job's raw nextest junit upload must set "
        "'if-no-files-found: warn': a compile failure leaves no junit, and "
        "that must stay a warning on an already-failing job rather than a new "
        "error or a silent skip"
    )

try:
    run_tests_index = next(
        index
        for index, step in enumerate(test_steps)
        if step_title(step) == "Run tests"
    )
except StopIteration:
    raise SystemExit("CI test job must keep the 'Run tests' step")
if raw_junit_index < run_tests_index:
    raise SystemExit(
        "CI test job's raw nextest junit upload must come after 'Run tests'"
    )
for step in test_steps[run_tests_index + 1 : raw_junit_index]:
    if not re.search(r"(?m)^\s+if: always\(\)\s*$", step):
        raise SystemExit(
            "CI test job must upload the raw nextest junit before "
            f"{step_title(step)!r}: that step can abort the job first and the "
            "junit would be lost again"
        )

# Uploading it is only half the contract - the artifact names have to be
# distinct per OS, or the second runner collides with the first.
OS_TERNARY = re.compile(
    r"\$\{\{ matrix\.name == 'Linux' && '([^']+)' \|\| '([^']+)' \}\}"
)


def expand_matrix_os(name: str) -> list[str]:
    match = OS_TERNARY.search(name)
    if match is None:
        return [name]
    return [OS_TERNARY.sub(value, name) for value in match.groups()]


raw_junit_name = re.search(r"(?m)^\s+name: (.+)$", with_mapping(raw_junit_step))
if raw_junit_name is None:
    raise SystemExit("CI test job's raw nextest junit upload must name its artifact")
raw_junit_names = expand_matrix_os(raw_junit_name.group(1).strip())
if len(raw_junit_names) != 2:
    raise SystemExit(
        "CI test job's raw nextest junit artifact name must vary by OS "
        f"(found {raw_junit_name.group(1).strip()!r}); the Linux and macOS "
        "runners share this job and would otherwise collide on one name"
    )

uploaded: dict[str, str] = {}
for job_name in re.findall(r"(?m)^  ([A-Za-z0-9_-]+):$", jobs_section):
    for step in steps_of(job_block(ci, job_name)):
        if "actions/upload-artifact@" not in step:
            continue
        name_match = re.search(r"(?m)^\s+name: (.+)$", with_mapping(step))
        if name_match is None:
            continue
        for artifact in expand_matrix_os(name_match.group(1).strip()):
            owner = f"{job_name} / {step_title(step)}"
            if artifact in uploaded and uploaded[artifact] != owner:
                raise SystemExit(
                    f"CI uploads artifact {artifact!r} from two different "
                    f"steps ({uploaded[artifact]} and {owner}); "
                    "upload-artifact@v4 rejects duplicate names"
                )
            uploaded[artifact] = owner

# The junit consumers download by glob. A raw artifact that drifts into one of
# those patterns would feed unvalidated evidence into a strict packet gate.
for consumer_pattern in re.findall(r"(?m)^\s+pattern: (\S*junit\S*)$", ci):
    for artifact in raw_junit_names:
        if fnmatch.fnmatch(artifact, consumer_pattern):
            raise SystemExit(
                f"raw nextest junit artifact {artifact!r} matches the "
                f"download pattern {consumer_pattern!r}; the raw upload is "
                "unvalidated retention and must stay out of the packet "
                "aggregation inputs"
            )

# --------------------------------------------------------------------------
# The workflow-contract tests must protect master, not just pull requests.
# --------------------------------------------------------------------------
drift_job = job_block(ci, "release-version-drift")
if "bash tests/dashboard_workflow_contract_test.sh" not in drift_job:
    raise SystemExit(
        "release-version-drift must keep running this contract test"
    )
if "github.event_name == 'push'" not in drift_job:
    raise SystemExit(
        "release-version-drift must run on push as well as pull_request, or "
        "these contract checks never run on master"
    )
if not re.search(r"(?m)^  push:\s*\n\s+branches: \[master\]", plugin):
    raise SystemExit("plugin validation must run on master pushes")
cursor_job = job_block(plugin, "cursor-native-extension")
for required in ["npm ci", "npm run check", "npm test", "npm run package"]:
    if required not in cursor_job:
        raise SystemExit(f"Cursor extension job must preserve {required!r}")

# A boundary step wired to an empty rule set passes every time and proves
# nothing, so the gate's contents are part of the contract, not just its
# invocation. Plan 11's acceptance names the semantics renderers may not
# compute; each id below carries one of them.
package_json = pathlib.Path("dashboard/package.json").read_text(encoding="utf-8")
if '"boundary:check"' not in package_json:
    raise SystemExit("dashboard/package.json must define the boundary:check script")

sgconfig = pathlib.Path("sgconfig.yml")
if not sgconfig.is_file():
    raise SystemExit("sgconfig.yml must exist for the renderer boundary gate")
rule_dir = pathlib.Path("tools/ast-grep/rules")
if rule_dir.as_posix() not in sgconfig.read_text(encoding="utf-8"):
    raise SystemExit(f"sgconfig.yml must list {rule_dir.as_posix()} in ruleDirs")

rules = "".join(
    path.read_text(encoding="utf-8") for path in sorted(rule_dir.glob("*.yml"))
)
for rule_id in [
    "viz-renderer-imports-server-state",
    "viz-renderer-opens-transport",
    "dashboard-ad-hoc-eventsource",
    "viz-renderer-persists-adapter-state",
    "viz-renderer-grades-state",
    "viz-adapter-ranks-locally",
    "viz-renderer-owns-routes",
]:
    # ast-grep resolves .ts and .tsx as separate languages and applies neither
    # rule to the other file type, so a rule that lost its Tsx twin would stop
    # covering GraphCanvas.tsx and Chart.tsx while still reporting success.
    for suffix in ["", "-tsx"]:
        full_id = f"{rule_id}{suffix}"
        if f"id: {full_id}\n" not in rules:
            raise SystemExit(
                f"{rule_dir.as_posix()} must keep Plan 11 boundary rule {full_id!r}"
            )
PY
