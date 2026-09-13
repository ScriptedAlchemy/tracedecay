#!/usr/bin/env bash
# Release safety guards.
#
# These are the release-workflow properties whose violation is silent and
# expensive: a suppressed downstream release, a publication cancelled halfway,
# a mutable third-party action inside the publish path, or a
# pull_request_target guard that hands out write credentials. Everything else
# about how these workflows are spelled is free to change.
set -euo pipefail

release_please=".github/workflows/release-please.yml"
release_stable=".github/workflows/release.yml"
release_beta=".github/workflows/release-beta.yml"
release_pr_integrity=".github/workflows/release-pr-integrity.yml"
sdk_conformance=".github/workflows/sdk-conformance.yml"

python3 - <<'PY'
import json
import tomllib
from pathlib import Path

PRODUCT_PACKAGE = "tracedecay"

with Path("Cargo.toml").open("rb") as handle:
    root = tomllib.load(handle)

version = Path("version.txt").read_text(encoding="utf-8").strip()
release_manifest_path = Path(
    ".release-please-manifest-beta.json"
    if "-" in version
    else ".release-please-manifest.json"
)
release_manifest = json.loads(
    release_manifest_path.read_text(encoding="utf-8")
)
server_manifest = json.loads(Path("server.json").read_text(encoding="utf-8"))
# The repository root is a virtual workspace manifest with no package of its
# own. The released version is the workspace one every member inherits, and the
# privacy invariant is carried by the per-member loop below, which now includes
# the product package.
if "package" in root:
    raise SystemExit("repository root must remain a virtual workspace manifest")
if (
    root["workspace"]["package"]["version"] != version
    or release_manifest.get(".") != version
    or server_manifest.get("version") != version
):
    raise SystemExit(
        f"release version authorities are not aligned with {release_manifest_path}"
    )
with Path("Cargo.lock").open("rb") as handle:
    lockfile = tomllib.load(handle)
product_locks = [
    package
    for package in lockfile["package"]
    if package.get("name") == PRODUCT_PACKAGE
]
if len(product_locks) != 1 or product_locks[0].get("version") != version:
    raise SystemExit("Cargo.lock product version is not aligned")

for member in root["workspace"]["members"]:
    manifest_path = Path(member, "Cargo.toml")
    with manifest_path.open("rb") as handle:
        manifest = tomllib.load(handle)
    if manifest["package"].get("publish") is not False:
        raise SystemExit(f"workspace package is publishable: {manifest_path}")
if PRODUCT_PACKAGE not in {Path(member).name for member in root["workspace"]["members"]}:
    raise SystemExit(f"workspace does not contain the product package {PRODUCT_PACKAGE}")
PY

python3 - <<'PY'
import json
from pathlib import Path

config = json.loads(
    Path("release-please-config-beta.json").read_text(encoding="utf-8")
)
if config.get("draft-pull-request") is not True:
    raise SystemExit(
        "beta release PRs must remain draft while the generated lockfile is updated"
    )
PY

# GitHub suppresses `on: release` workflows for releases created by
# GITHUB_TOKEN, so Release Please must use the dedicated release token.
if grep -q 'token: ${{ secrets.GITHUB_TOKEN }}' "$release_please"; then
  echo "Release Please must not publish releases with GITHUB_TOKEN" >&2
  exit 1
fi

python3 - "$release_please" "$release_stable" "$release_beta" <<'PY'
import sys

for path in sys.argv[1:]:
    text = open(path, encoding="utf-8").read()
    if "cancel-in-progress: true" in text:
        raise SystemExit(f"{path} must never cancel in-progress publication")
PY

python3 - "$release_please" "$release_stable" "$release_beta" \
  "$release_pr_integrity" "$sdk_conformance" <<'PY'
import re
import sys

sha_ref = re.compile(r"^[^@]+@[0-9a-f]{40}$")
for path in sys.argv[1:]:
    text = open(path, encoding="utf-8").read()
    for uses in re.findall(r"^\s*-?\s*uses:\s+([^#\s]+)", text, re.MULTILINE):
        if uses.startswith("./"):
            continue
        if not sha_ref.fullmatch(uses):
            raise SystemExit(
                f"{path} external action must use an immutable SHA: {uses}"
            )
PY

python3 - "$release_stable" "$release_beta" <<'PY'
import re
import sys

stable_path, beta_path = sys.argv[1:]
stable = open(stable_path, encoding="utf-8").read()
beta = open(beta_path, encoding="utf-8").read()

for path, text, job, next_job in (
    (stable_path, stable, "validate-release", "dashboard-assets"),
    (beta_path, beta, "validate", "build"),
):
    section = text.split(f"  {job}:\n", 1)[1].split(f"\n  {next_job}:", 1)[0]
    match = re.search(r"^    permissions:\n((?:^      .+\n)+)", section, re.MULTILINE)
    if match is None:
        raise SystemExit(f"{path} {job} must declare job-level permissions")
    permissions = {
        line.strip()
        for line in match.group(1).splitlines()
        if line.strip()
    }
    if permissions != {"contents: read", "attestations: read"}:
        raise SystemExit(
            f"{path} {job} must grant exactly contents: read and attestations: read"
        )

external_publication_markers = (
    "homebrew-tap",
    "scoop-bucket",
    ".bottle.tar.gz",
    "update-homebrew:",
    "update-scoop:",
    "TAP_GITHUB_TOKEN",
)
for marker in external_publication_markers:
    if marker in stable:
        raise SystemExit(
            f"{stable_path} must not publish external package repositories: {marker}"
        )

for path, text in ((stable_path, stable), (beta_path, beta)):
    release_test = re.search(
        r"- name: Test release distribution\n"
        r"\s+if: matrix\.name == 'x86_64-linux'\n"
        r"\s+run: (?:(?!- name:)[\s\S])*?"
        r"cargo test --workspace --release --target",
        text,
    )
    if release_test is None:
        raise SystemExit(
            f"{path} must run the full release test suite once on x86_64-linux"
        )
    if "scripts/package-release-archive.py" not in text:
        raise SystemExit(f"{path} must use deterministic release archive packaging")
    for mutable_packager in ("tar czf", "tar -czf", "Compress-Archive", "7z a "):
        if mutable_packager in text:
            raise SystemExit(
                f"{path} contains timestamp-sensitive packaging: {mutable_packager}"
            )
    for required in (
        "scripts/plan-release-recovery.py",
        "scripts/verify-retained-release-assets.sh",
        "--tag",
        "--repo",
        "--signer-workflow",
        "--source-digest",
        "outputs.build_required",
        'test "$GITHUB_REF" = "refs/tags/',
        'test "$GITHUB_SHA" = "$source_sha"',
    ):
        if required not in text:
            raise SystemExit(
                f"{path} must retain uploaded assets with exact source provenance: "
                f"{required}"
            )

for forbidden in (
    'cmp -s "$asset" "remote-assets/$name"',
    'cmp -s "$release_asset" "remote-assets/$name"',
):
    if forbidden in stable or forbidden in beta:
        raise SystemExit(
            "release recovery must not compare rebuilt mutable outputs: "
            f"{forbidden}"
        )
PY

# Exercise the canonical verifier rather than requiring every workflow to copy
# its `gh attestation verify` implementation. This keeps the workflow guard
# focused on delegation while proving the shared authority derives the exact
# tag source ref, preserves the source digest and signer, rejects self-hosted
# attestations, and propagates verification failures.
python3 - <<'PY'
import json
import os
import stat
import subprocess
import tempfile
from pathlib import Path

root = Path.cwd()
verifier = root / "scripts/verify-retained-release-assets.sh"
tag = "v9.8.7"
repo = "ScriptedAlchemy/tracedecay"
signer = "ScriptedAlchemy/tracedecay/.github/workflows/release.yml"
source_digest = "0123456789abcdef"

with tempfile.TemporaryDirectory() as temp:
    temp_path = Path(temp)
    fake_bin = temp_path / "bin"
    fake_bin.mkdir()
    invocation_log = temp_path / "gh-invocations.jsonl"
    fake_gh = fake_bin / "gh"
    fake_gh.write_text(
        """#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

arguments = sys.argv[1:]
with Path(os.environ["GH_INVOCATION_LOG"]).open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(arguments) + "\\n")

if arguments[:2] == ["release", "download"]:
    pattern = arguments[arguments.index("--pattern") + 1]
    destination = Path(arguments[arguments.index("--dir") + 1])
    destination.mkdir(parents=True, exist_ok=True)
    (destination / pattern).write_bytes(b"retained release asset")

if (
    arguments[:2] == ["attestation", "verify"]
    and os.environ.get("GH_FAIL_ATTESTATION") == "1"
):
    raise SystemExit(17)
""",
        encoding="utf-8",
    )
    fake_gh.chmod(fake_gh.stat().st_mode | stat.S_IXUSR)

    environment = os.environ.copy()
    environment["PATH"] = f"{fake_bin}:{environment['PATH']}"
    environment["GH_INVOCATION_LOG"] = str(invocation_log)

    files = [temp_path / "first.tar.gz", temp_path / "second.mcpb"]
    for file in files:
        file.write_bytes(b"release asset")

    command = [
        str(verifier),
        "--tag",
        tag,
        "--repo",
        repo,
        "--signer-workflow",
        signer,
        "--source-digest",
        source_digest,
        "--files",
        *(str(file) for file in files),
    ]
    subprocess.run(command, cwd=root, env=environment, check=True)

    invocations = [
        json.loads(line)
        for line in invocation_log.read_text(encoding="utf-8").splitlines()
    ]
    expected_suffix = [
        "--repo",
        repo,
        "--signer-workflow",
        signer,
        "--source-ref",
        f"refs/tags/{tag}",
        "--source-digest",
        source_digest,
        "--deny-self-hosted-runners",
    ]
    expected = [
        ["attestation", "verify", str(file), *expected_suffix]
        for file in files
    ]
    if invocations != expected:
        raise SystemExit(
            "canonical release verifier did not preserve exact provenance: "
            f"{invocations!r}"
        )

    failure_environment = environment.copy()
    failure_environment["GH_FAIL_ATTESTATION"] = "1"
    failed = subprocess.run(
        command[:-1],
        cwd=root,
        env=failure_environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if failed.returncode == 0:
        raise SystemExit("canonical release verifier swallowed attestation failure")
PY

python3 - "$release_pr_integrity" <<'PY'
import sys

path = sys.argv[1]
text = open(path, encoding="utf-8").read()

# This workflow runs on pull_request_target, so it sees fork code with the
# base repository's token. It must never hand that token to the checkout, and
# must never hold write scopes.
if "persist-credentials: false" not in text:
    raise SystemExit(f"{path} must check out without persisted credentials")
if "contents: write" in text or "pull-requests: write" in text:
    raise SystemExit(f"{path} must remain read-only")
PY
