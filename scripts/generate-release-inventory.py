#!/usr/bin/env python3
"""Generate a deterministic SPDX inventory for a shipped CLI release target."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import tarfile
import tomllib
import zipfile
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import quote


TARGET = "//crates/tracedecay-cli:tracedecay"
FONT_LICENSES = {
    "@fontsource-variable/archivo": "OFL-1.1",
    "@fontsource-variable/ibm-plex-sans": "OFL-1.1",
    "@fontsource/ibm-plex-mono": "OFL-1.1",
}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def canonical_json(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode()


def label_name(label: str) -> str:
    label = label.split(" (", 1)[0]
    return label.strip('"')


def cquery(source_root: Path) -> tuple[str, str]:
    env = os.environ.copy()
    graph = subprocess.run(
        [
            "bazel",
            "cquery",
            "--config=dist",
            "--config=ci",
            f"deps({TARGET})",
            "--output=graph",
        ],
        cwd=source_root,
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    kinds = subprocess.run(
        [
            "bazel",
            "cquery",
            "--config=dist",
            "--config=ci",
            f"deps({TARGET})",
            "--output=label_kind",
        ],
        cwd=source_root,
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    return graph.stdout, kinds.stdout


def parse_graph(graph: str, kinds: str) -> list[dict[str, str]]:
    target_kinds: dict[str, str] = {}
    for line in kinds.splitlines():
        match = re.match(r"(.+?) rule (.+)$", line.strip())
        if match:
            target_kinds[label_name(match.group(2))] = match.group(1)

    edges = [
        (label_name(parent), label_name(child))
        for parent, child in re.findall(r'"([^"]+)"\s*->\s*"([^"]+)"', graph)
    ]
    adjacency: dict[str, list[str]] = {}
    for parent, child in edges:
        adjacency.setdefault(parent, []).append(child)

    root = label_name(TARGET)
    pending = [root]
    visited: set[str] = set()
    included: set[str] = set()
    while pending:
        current = pending.pop()
        if current in visited:
            continue
        visited.add(current)
        kind = target_kinds.get(current, "")
        if kind in {"cargo_build_script", "rust_proc_macro", "rust_test"}:
            continue
        if kind == "rust_library":
            if current.startswith("@@") or current.startswith("@"):
                included.add(current)
            elif current.startswith("//crates/") or current.startswith("//patches/"):
                included.add(current)
        pending.extend(adjacency.get(current, ()))

    return [{"label": label, "kind": target_kinds.get(label, "")} for label in sorted(included)]


def lock_package_index(source_root: Path) -> tuple[dict[str, dict[str, object]], dict[str, dict[str, object]]]:
    lock = tomllib.loads((source_root / "Cargo.lock").read_text(encoding="utf-8"))
    by_repo: dict[str, dict[str, object]] = {}
    by_name: dict[str, dict[str, object]] = {}
    for package in lock.get("package", []):
        if package.get("source"):
            by_repo[f"c__{package['name']}-{package['version']}"] = package
        else:
            by_name[package["name"]] = package
    return by_repo, by_name


def cargo_package(
    source_root: Path,
    output_base: Path,
    label: str,
    lock_packages: tuple[dict[str, dict[str, object]], dict[str, dict[str, object]]],
) -> dict[str, object] | None:
    by_repo, by_name = lock_packages
    if label.startswith("@"):
        repository = label.split("//", 1)[0].lstrip("@")
        repository = repository.split("+", 1)[0] if repository.startswith("c+") else repository
        package = by_repo.get(repository)
        if package is None:
            # Canonical repository labels may include a module-version suffix.
            package = next(
                (
                    value
                    for key, value in by_repo.items()
                    if repository.endswith(key.removeprefix("c__"))
                ),
                None,
            )
        if package is None:
            return {
                "name": repository,
                "version": "NOASSERTION",
                "source": "NOASSERTION",
                "license": "NOASSERTION",
                "checksum": None,
                "local_path": None,
            }
        manifest = output_base / "external" / repository / "Cargo.toml"
        source = str(package.get("source", "NOASSERTION"))
        return package_record(package, manifest, source_root, source)

    package_path = label.split("//", 1)[-1].split(":", 1)[0]
    if not package_path:
        return None
    manifest = source_root / package_path / "Cargo.toml"
    if not manifest.is_file():
        return None
    with manifest.open("rb") as source_file:
        metadata = tomllib.load(source_file).get("package", {})
    package_name = str(metadata.get("name", ""))
    package = by_name.get(package_name, {})
    source = f"git+https://github.com/ScriptedAlchemy/tracedecay#source-revision"
    local_path = package_path
    return package_record(
        {
            "name": package_name,
            "version": metadata.get("version", package.get("version", "NOASSERTION")),
            "checksum": package.get("checksum"),
        },
        manifest,
        source_root,
        source,
        local_path=local_path,
    )


def package_record(
    package: dict[str, object],
    manifest: Path,
    source_root: Path,
    source: str,
    *,
    local_path: str | None = None,
) -> dict[str, object]:
    metadata: dict[str, object] = {}
    if manifest.is_file():
        with manifest.open("rb") as source_file:
            metadata = tomllib.load(source_file).get("package", {})
    license_value = metadata.get("license")
    name = str(package.get("name", metadata.get("name", "unknown")))
    version = str(package.get("version", metadata.get("version", "NOASSERTION")))
    source_url = source.removeprefix("git+")
    if source.startswith("registry+"):
        source_url = f"https://crates.io/crates/{quote(name, safe='')}/{quote(version, safe='')}"
    record: dict[str, object] = {
        "name": name,
        "version": version,
        "source": source,
        "downloadLocation": source_url or "NOASSERTION",
        "license": license_value if isinstance(license_value, str) and license_value else "NOASSERTION",
        "checksum": package.get("checksum"),
        "purl": f"pkg:cargo/{quote(name, safe='')}@{quote(version, safe='')}",
    }
    if local_path:
        record["local_path"] = local_path
        record["source"] = f"{local_path} at the release source revision"
        record["downloadLocation"] = "NOASSERTION"
    return record


def component_id(purl: str) -> str:
    return "SPDXRef-" + hashlib.sha256(purl.encode()).hexdigest()[:24]


def checksum_from_sri(integrity: str) -> dict[str, str] | None:
    for item in integrity.split():
        algorithm, separator, encoded = item.partition("-")
        if not separator or algorithm.lower() not in {"sha1", "sha256", "sha384", "sha512"}:
            continue
        import base64

        try:
            digest = base64.b64decode(encoded, validate=True).hex()
        except ValueError:
            continue
        return {"algorithm": algorithm.upper().replace("SHA", "SHA"), "checksumValue": digest}
    return None


def npm_component_key(name: str, version: str, package_keys: set[str]) -> str | None:
    exact = f"{name}@{version}"
    if exact in package_keys:
        return exact
    candidates = sorted(
        key
        for key in package_keys
        if key.startswith(exact + "(")
    )
    return candidates[0] if candidates else None


def npm_components(source_root: Path, platform: str) -> list[dict[str, object]]:
    try:
        import yaml
    except ImportError as error:
        raise RuntimeError("PyYAML is required to read the locked dashboard package graph") from error

    lock = yaml.safe_load((source_root / "pnpm-lock.yaml").read_text(encoding="utf-8"))
    dashboard = lock.get("importers", {}).get("dashboard", {})
    roots = dashboard.get("dependencies", {})
    packages = lock.get("packages", {})
    snapshots = lock.get("snapshots", {})
    package_keys = set(packages)
    os_name, arch = {
        "aarch64-macos": ("darwin", "arm64"),
        "x86_64-linux": ("linux", "x64"),
        "aarch64-linux": ("linux", "arm64"),
        "x86_64-windows": ("win32", "x64"),
    }[platform]

    pending: list[str] = []
    for name, record in roots.items():
        version = record.get("version")
        if version:
            key = npm_component_key(name, str(version), package_keys)
            if key:
                pending.append(key)

    visited: set[str] = set()
    result: list[dict[str, object]] = []
    while pending:
        key = pending.pop()
        if key in visited:
            continue
        visited.add(key)
        metadata = packages.get(key, {})
        version = key.rsplit("@", 1)[-1].split("(", 1)[0]
        name = key[: -(len(key.rsplit("@", 1)[-1]) + 1)]
        if not name:
            continue
        package = {
            "name": name,
            "version": version,
            "source": metadata.get("resolution", {}).get("tarball")
            or f"https://registry.npmjs.org/{quote(name, safe='@/')}/-/{name.rsplit('/', 1)[-1]}-{version}.tgz",
            "license": FONT_LICENSES.get(name, "NOASSERTION"),
            "checksum": checksum_from_sri(
                str(metadata.get("resolution", {}).get("integrity", ""))
            ),
            "purl": f"pkg:npm/{quote(name, safe='@/') }@{quote(version, safe='')}",
        }
        result.append(package)
        node = snapshots.get(key, {})
        for group in ("dependencies", "optionalDependencies"):
            for dependency, child_version in node.get(group, {}).items():
                child = npm_component_key(dependency, str(child_version), package_keys)
                if child is None:
                    continue
                constraints = packages.get(child, {})
                if constraints.get("os") and os_name not in constraints["os"] and "!" + os_name not in constraints["os"]:
                    continue
                if constraints.get("cpu") and arch not in constraints["cpu"] and "!" + arch not in constraints["cpu"]:
                    continue
                pending.append(child)
    return sorted(result, key=lambda component: str(component["purl"]))


def scope_inventory(
    source_root: Path,
    output: Path,
    platform: str,
    source_sha: str,
    dashboard_dist: Path,
    graph_file: Path | None,
    kinds_file: Path | None,
    output_base_arg: Path | None,
) -> None:
    if platform not in {
        "aarch64-macos",
        "x86_64-linux",
        "aarch64-linux",
        "x86_64-windows",
    }:
        raise ValueError(f"unsupported release platform: {platform}")
    if graph_file and kinds_file:
        graph = graph_file.read_text(encoding="utf-8")
        kinds = kinds_file.read_text(encoding="utf-8")
    else:
        graph, kinds = cquery(source_root)
    labels = parse_graph(graph, kinds)
    output_base = output_base_arg
    if output_base is None:
        output_base = Path(
            subprocess.run(
                ["bazel", "info", "--config=dist", "--config=ci", "output_base"],
                cwd=source_root,
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
        )
    cargo_lock = lock_package_index(source_root)
    rust_components = {
        str(record["purl"]): record
        for item in labels
        if (
            record := cargo_package(source_root, output_base, item["label"], cargo_lock)
        )
        is not None
    }
    assets = []
    if not dashboard_dist.is_dir():
        raise FileNotFoundError(f"built dashboard asset directory is missing: {dashboard_dist}")
    for path in sorted(item for item in dashboard_dist.rglob("*") if item.is_file()):
        assets.append(
            {
                "path": path.relative_to(dashboard_dist).as_posix(),
                "sha256": sha256(path),
                "size": path.stat().st_size,
            }
        )
    adapted_sources = []
    module_file = source_root / "MODULE.bazel"
    if module_file.is_file():
        module_text = module_file.read_text(encoding="utf-8")
        patch_paths = sorted(set(re.findall(r'"//scripts:([^"]+\.patch)"', module_text)))
        for relative in patch_paths:
            path = source_root / "scripts" / relative
            if path.is_file():
                adapted_sources.append(
                    {
                        "path": path.relative_to(source_root).as_posix(),
                        "sha256": sha256(path),
                    }
                )
    scope = {
        "format": "tracedecay-release-component-scope-v1",
        "platform": platform,
        "bazel_target": TARGET,
        "source_sha": source_sha,
        "rust_components": [rust_components[key] for key in sorted(rust_components)],
        "dashboard_assets": assets,
        "adapted_sources": adapted_sources,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(canonical_json(scope))


def artifact_binary(archive: Path, platform: str) -> bytes:
    member = "tracedecay.exe" if platform == "x86_64-windows" else "tracedecay"
    if archive.name.endswith(".zip"):
        with zipfile.ZipFile(archive) as packaged:
            return packaged.read(member)
    with tarfile.open(archive, "r:gz") as packaged:
        content = packaged.extractfile(member)
        if content is None:
            raise ValueError(f"archive does not contain {member}")
        return content.read()


def spdx_package(record: dict[str, object]) -> dict[str, object]:
    purl = str(record["purl"])
    license_value = str(record.get("license") or "NOASSERTION")
    package: dict[str, object] = {
        "name": record["name"],
        "SPDXID": component_id(purl),
        "versionInfo": str(record.get("version") or "NOASSERTION"),
        "downloadLocation": str(record.get("downloadLocation") or record.get("source") or "NOASSERTION"),
        "filesAnalyzed": False,
        "licenseConcluded": "NOASSERTION",
        "licenseDeclared": license_value,
        "copyrightText": "NOASSERTION",
        "externalRefs": [
            {
                "referenceCategory": "PACKAGE-MANAGER",
                "referenceType": "purl",
                "referenceLocator": purl,
            }
        ],
    }
    checksum = record.get("checksum")
    if isinstance(checksum, dict):
        package["checksums"] = [checksum]
    elif isinstance(checksum, str) and checksum:
        package["checksums"] = [{"algorithm": "SHA256", "checksumValue": checksum}]
    source_info = record.get("source")
    if source_info and source_info != package["downloadLocation"]:
        package["sourceInfo"] = str(source_info)
    return package


def release_inventory(
    source_root: Path,
    scope_path: Path,
    archive: Path,
    mcpb: Path,
    tag: str,
    source_sha: str,
    source_epoch: int,
    repository: str,
    output: Path,
) -> None:
    scope = json.loads(scope_path.read_text(encoding="utf-8"))
    platform = str(scope["platform"])
    npm = npm_components(source_root, platform)
    components = [*scope["rust_components"], *npm]
    packages = [spdx_package(item) for item in components]
    package_ids = {str(package["SPDXID"]) for package in packages}
    archive_package_id = "SPDXRef-Archive"
    mcpb_package_id = "SPDXRef-MCPB"
    binary_id = "SPDXRef-CLI-Binary"
    asset_package = lambda name, path, identifier: {
        "name": name,
        "SPDXID": identifier,
        "versionInfo": tag.removeprefix("v"),
        "downloadLocation": f"https://github.com/{repository}/releases/download/{quote(tag, safe='')}/{quote(path.name, safe='.-_')}",
        "filesAnalyzed": False,
        "licenseConcluded": "NOASSERTION",
        "licenseDeclared": "NOASSERTION",
        "copyrightText": "NOASSERTION",
        "checksums": [{"algorithm": "SHA256", "checksumValue": sha256(path)}],
    }
    packages.extend(
        [
            asset_package(archive.name, archive, archive_package_id),
            asset_package(mcpb.name, mcpb, mcpb_package_id),
            {
                "name": "tracedecay CLI binary",
                "SPDXID": binary_id,
                "versionInfo": tag.removeprefix("v"),
                "downloadLocation": f"https://github.com/{repository}",
                "filesAnalyzed": False,
                "licenseConcluded": "NOASSERTION",
                "licenseDeclared": "MIT",
                "copyrightText": "NOASSERTION",
                "checksums": [
                    {
                        "algorithm": "SHA256",
                        "checksumValue": hashlib.sha256(
                            artifact_binary(archive, platform)
                        ).hexdigest(),
                    }
                ],
            },
        ]
    )
    file_entries = []
    for item in scope["dashboard_assets"]:
        path = str(item["path"])
        file_id = "SPDXRef-Asset-" + hashlib.sha256(path.encode()).hexdigest()[:20]
        font_package = next(
            (
                name
                for name in FONT_LICENSES
                if path.startswith(name.split("/", 1)[-1] + "-")
                or name.split("/", 1)[-1] in path
            ),
            None,
        )
        file_entries.append(
            {
                "fileName": f"dashboard/app-dist/{path}",
                "SPDXID": file_id,
                "checksums": [{"algorithm": "SHA256", "checksumValue": item["sha256"]}],
                "licenseConcluded": "NOASSERTION",
                "licenseInfoInFiles": [FONT_LICENSES[font_package]] if font_package else ["NOASSERTION"],
                "copyrightText": "NOASSERTION",
            }
        )

    relationships = [
        {"spdxElementId": archive_package_id, "relationshipType": "CONTAINS", "relatedSpdxElement": binary_id},
        {"spdxElementId": mcpb_package_id, "relationshipType": "CONTAINS", "relatedSpdxElement": binary_id},
    ]
    for identifier in sorted(package_ids):
        relationships.extend(
            [
                {"spdxElementId": binary_id, "relationshipType": "DEPENDS_ON", "relatedSpdxElement": identifier},
                {"spdxElementId": archive_package_id, "relationshipType": "CONTAINS", "relatedSpdxElement": identifier},
                {"spdxElementId": mcpb_package_id, "relationshipType": "CONTAINS", "relatedSpdxElement": identifier},
            ]
        )
    for item in file_entries:
        for parent in (binary_id, archive_package_id, mcpb_package_id):
            relationships.append(
                {
                    "spdxElementId": parent,
                    "relationshipType": "CONTAINS",
                    "relatedSpdxElement": item["SPDXID"],
                }
            )
    review_items = sorted(
        f"{package['name']}@{package.get('versionInfo', 'NOASSERTION')}"
        for package in packages
        if package.get("licenseDeclared") == "NOASSERTION"
        or package.get("downloadLocation") == "NOASSERTION"
    )
    annotations = [
        {
            "annotationDate": datetime.fromtimestamp(
                source_epoch, timezone.utc
            ).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "annotationType": "OTHER",
            "annotator": "Tool: tracedecay-release-inventory",
            "comment": f"Review required: license or source metadata is unknown for {item}.",
        }
        for item in review_items
    ]
    file_entries.extend(
        {
            "fileName": str(item["path"]),
            "SPDXID": "SPDXRef-Adapted-" + hashlib.sha256(str(item["path"]).encode()).hexdigest()[:20],
            "checksums": [{"algorithm": "SHA256", "checksumValue": item["sha256"]}],
            "licenseConcluded": "NOASSERTION",
            "licenseInfoInFiles": ["NOASSERTION"],
            "copyrightText": "NOASSERTION",
        }
        for item in scope.get("adapted_sources", [])
    )
    document_name = f"TraceDecay {tag} {platform} component inventory"
    namespace = (
        "https://spdx.org/spdxdocs/"
        + quote(document_name.replace(" ", "-"), safe="-_.")
        + "/"
        + hashlib.sha256((source_sha + sha256(archive) + sha256(mcpb)).encode()).hexdigest()
    )
    document = {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": document_name,
        "documentNamespace": namespace,
        "creationInfo": {
            "creators": ["Tool: tracedecay-release-inventory"],
            "created": datetime.fromtimestamp(
                source_epoch, timezone.utc
            ).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "comment": f"Source revision {source_sha}; Bazel target {scope['bazel_target']}. Unknown metadata is called out in annotations.",
        },
        "packages": sorted(packages, key=lambda package: str(package["SPDXID"])),
        "files": sorted(file_entries, key=lambda item: str(item["SPDXID"])),
        "relationships": sorted(
            relationships,
            key=lambda item: (
                item["spdxElementId"],
                item["relationshipType"],
                item["relatedSpdxElement"],
            ),
        ),
        "annotations": annotations,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(canonical_json(document))


def main() -> None:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    scope = commands.add_parser("scope")
    scope.add_argument("--source-root", type=Path, default=Path("."))
    scope.add_argument("--platform", required=True)
    scope.add_argument("--source-sha", required=True)
    scope.add_argument("--dashboard-dist", type=Path, required=True)
    scope.add_argument("--graph", type=Path)
    scope.add_argument("--kinds", type=Path)
    scope.add_argument("--output-base", type=Path)
    scope.add_argument("--output", type=Path, required=True)
    document = commands.add_parser("document")
    document.add_argument("--source-root", type=Path, default=Path("."))
    document.add_argument("--scope", type=Path, required=True)
    document.add_argument("--archive", type=Path, required=True)
    document.add_argument("--mcpb", type=Path, required=True)
    document.add_argument("--tag", required=True)
    document.add_argument("--source-sha", required=True)
    document.add_argument("--source-epoch", type=int, required=True)
    document.add_argument("--repository", required=True)
    document.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "scope":
        scope_inventory(
            args.source_root,
            args.output,
            args.platform,
            args.source_sha,
            args.dashboard_dist,
            args.graph,
            args.kinds,
            args.output_base,
        )
    else:
        release_inventory(
            args.source_root,
            args.scope,
            args.archive,
            args.mcpb,
            args.tag,
            args.source_sha,
            args.source_epoch,
            args.repository,
            args.output,
        )


if __name__ == "__main__":
    main()
