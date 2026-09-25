#!/usr/bin/env bash
# Release safety guards.
#
# Release version authorities must agree, the default release-please channel
# must propose prereleases, and the canonical retained-asset verifier must
# preserve exact attestation provenance and propagate verification failures.
set -euo pipefail

python3 - <<'PY'
import json
import tomllib
from pathlib import Path

PRODUCT_PACKAGE = "tracedecay"

with Path("Cargo.toml").open("rb") as handle:
    root = tomllib.load(handle)

version = Path("version.txt").read_text(encoding="utf-8").strip()
release_manifest_path = Path(".release-please-manifest.json")
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

beta = json.loads(Path("release-please-config.json").read_text(encoding="utf-8"))
stable = json.loads(
    Path("release-please-config-stable.json").read_text(encoding="utf-8")
)
# The default channel, what every push to master proposes, must be a
# prerelease. A stable release is only reachable through the explicit dispatch
# that selects the stable config.
if beta.get("versioning") != "prerelease" or beta.get("prerelease") is not True:
    raise SystemExit("release-please-config.json must propose prereleases")
if stable.get("prerelease") or stable.get("versioning") == "prerelease":
    raise SystemExit("release-please-config-stable.json must publish full releases")
for path, config in (
    ("release-please-config.json", beta),
    ("release-please-config-stable.json", stable),
):
    if config.get("draft-pull-request") is not True:
        raise SystemExit(
            f"{path}: release PRs must remain draft while the generated lockfile is updated"
        )
sdk_paths = [
    item.get("path", "")
    for item in beta["packages"]["."]["extra-files"]
    if str(item.get("path", "")).startswith("sdks/")
]
if sdk_paths:
    raise SystemExit(
        "beta release-please must not bump the independently versioned SDK: "
        + ", ".join(sdk_paths)
    )
PY

# Exercise the canonical verifier rather than requiring every workflow to copy
# its `gh attestation verify` implementation. A fake `gh` serves one
# attestation digest per signer ref and a compare status per commit range, so
# each case asserts the verifier's accept/reject decision for one provenance
# shape, including the master-dispatched run whose attestation names master's
# head rather than the tag commit.
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
tag_sha = "0123456789abcdef"
master_head = "fedcba9876543210"
tag_ref = f"refs/tags/{tag}"
master_ref = "refs/heads/master"

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

if arguments[:2] == ["attestation", "verify"]:
    if "--deny-self-hosted-runners" not in arguments:
        raise SystemExit(20)
    if arguments[arguments.index("--signer-workflow") + 1] != os.environ["GH_SIGNER"]:
        raise SystemExit(21)
    source_ref = arguments[arguments.index("--source-ref") + 1]
    digest = json.loads(os.environ["GH_ATTESTATIONS"]).get(source_ref)
    if digest is None:
        raise SystemExit(17)
    if (
        "--source-digest" in arguments
        and arguments[arguments.index("--source-digest") + 1] != digest
    ):
        raise SystemExit(19)
    print(json.dumps([
        {"verificationResult": {"signature": {"certificate": {
            "sourceRepositoryRef": source_ref,
            "sourceRepositoryDigest": digest,
        }}}}
    ]))
elif arguments[:1] == ["api"]:
    commit_range = arguments[1].rsplit("/compare/", 1)[1]
    print(json.loads(os.environ["GH_COMPARE"]).get(commit_range, "diverged"))
""",
        encoding="utf-8",
    )
    fake_gh.chmod(fake_gh.stat().st_mode | stat.S_IXUSR)

    files = [temp_path / "first.tar.gz", temp_path / "second.mcpb"]
    for file in files:
        file.write_bytes(b"release asset")

    def verify(attestations, signer_refs=(), compare=None):
        environment = os.environ.copy()
        environment["PATH"] = f"{fake_bin}:{environment['PATH']}"
        environment["GH_INVOCATION_LOG"] = str(invocation_log)
        environment["GH_SIGNER"] = signer
        environment["GH_ATTESTATIONS"] = json.dumps(attestations)
        environment["GH_COMPARE"] = json.dumps(compare or {})
        command = [
            str(verifier),
            "--tag", tag,
            "--repo", repo,
            "--signer-workflow", signer,
            "--source-digest", tag_sha,
        ]
        for signer_ref in signer_refs:
            command += ["--signer-ref", signer_ref]
        command += ["--files", *(str(file) for file in files)]
        return subprocess.run(
            command,
            cwd=root,
            env=environment,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        ).returncode == 0

    both_refs = (tag_ref, master_ref)
    descends = {f"{tag_sha}...{master_head}": "ahead"}
    cases = [
        ("tag-dispatched run attesting the tag commit", True,
         verify({tag_ref: tag_sha})),
        ("master-dispatched run attesting a master head that descends from the tag", True,
         verify({master_ref: master_head}, both_refs, descends)),
        ("master-dispatched run attesting the tag commit itself", True,
         verify({master_ref: tag_sha}, both_refs)),
        ("master-dispatched run attesting a head the tag is not an ancestor of", False,
         verify({master_ref: master_head}, both_refs, {f"{tag_sha}...{master_head}": "diverged"})),
        ("tag-ref attestation naming any commit but the tag", False,
         verify({tag_ref: master_head}, both_refs, descends)),
        ("master-ref attestation when only the tag ref is allowed", False,
         verify({master_ref: master_head}, (), descends)),
        ("no attestation for any allowed ref", False,
         verify({}, both_refs, descends)),
    ]
    wrong = [name for name, expected, accepted in cases if accepted != expected]
    if wrong:
        raise SystemExit("canonical release verifier decided wrongly for: " + "; ".join(wrong))
PY
