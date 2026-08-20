#!/usr/bin/env python3
"""Behavioral tests for the pinned release runtime preparer."""

from __future__ import annotations

import hashlib
import io
import json
import os
import subprocess
import tarfile
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
PREPARER = ROOT / "scripts" / "prepare-release-runtime.py"
ARCHIVE_ENTRY = "onnxruntime-linux-aarch64/lib/libonnxruntime.so.1.24.2"
PAYLOAD = b"portable official ONNX Runtime\n"
LICENSE_ENTRY = "onnxruntime-linux-aarch64/LICENSE"
LICENSE_PAYLOAD = b"ONNX Runtime license\n"
NOTICES_ENTRY = "onnxruntime-linux-aarch64/ThirdPartyNotices.txt"
NOTICES_PAYLOAD = b"ONNX Runtime third-party notices\n"


def build_archive(path: Path) -> str:
    with tarfile.open(path, "w:gz") as archive:
        entry = tarfile.TarInfo(ARCHIVE_ENTRY)
        entry.size = len(PAYLOAD)
        entry.mode = 0o755
        archive.addfile(entry, io.BytesIO(PAYLOAD))
        for name, payload in [
            (LICENSE_ENTRY, LICENSE_PAYLOAD),
            (NOTICES_ENTRY, NOTICES_PAYLOAD),
        ]:
            notice = tarfile.TarInfo(name)
            notice.size = len(payload)
            notice.mode = 0o644
            archive.addfile(notice, io.BytesIO(payload))
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_manifest(path: Path, archive: Path, digest: str) -> None:
    path.write_text(
        json.dumps(
            {
                "include": [
                    {
                        "name": "aarch64-macos",
                        "runner": "macos-14",
                        "target": "aarch64-apple-darwin",
                        "archive": "tar.gz",
                    },
                    {
                        "name": "aarch64-linux",
                        "runner": "ubuntu-22.04-arm",
                        "target": "aarch64-unknown-linux-gnu",
                        "archive": "tar.gz",
                        "runtime": {
                            "url": archive.as_uri(),
                            "sha256": digest,
                            "archive_entry": ARCHIVE_ENTRY,
                            "entry_name": "libonnxruntime.so.1",
                            "link_name": "libonnxruntime.so",
                            "notices": [
                                {
                                    "kind": "license",
                                    "archive_entry": LICENSE_ENTRY,
                                    "entry_name": "onnxruntime-LICENSE",
                                },
                                {
                                    "kind": "notices",
                                    "archive_entry": NOTICES_ENTRY,
                                    "entry_name": "onnxruntime-ThirdPartyNotices.txt",
                                },
                            ],
                        },
                    },
                ]
            }
        ),
        encoding="utf-8",
    )


def run_preparer(
    manifest: Path,
    target: str,
    output: Path,
    github_env: Path,
    github_output: Path,
) -> subprocess.CompletedProcess[str]:
    environment = os.environ.copy()
    environment.pop("LD_LIBRARY_PATH", None)
    environment.pop("RUSTFLAGS", None)
    return subprocess.run(
        [
            "python3",
            str(PREPARER),
            "--manifest",
            str(manifest),
            "--name",
            target,
            "--output",
            str(output),
            "--github-env",
            str(github_env),
            "--github-output",
            str(github_output),
        ],
        text=True,
        capture_output=True,
        env=environment,
    )


def main() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        archive = root / "runtime.tgz"
        digest = build_archive(archive)
        manifest = root / "targets.json"
        write_manifest(manifest, archive, digest)

        output = root / "runtime"
        github_env = root / "github-env"
        github_output = root / "github-output"
        result = run_preparer(
            manifest,
            "aarch64-linux",
            output,
            github_env,
            github_output,
        )
        assert result.returncode == 0, result.stderr
        runtime_library = output / "libonnxruntime.so.1"
        assert runtime_library.read_bytes() == PAYLOAD
        assert runtime_library.stat().st_mode & 0o777 == 0o644
        linker_name = output / "libonnxruntime.so"
        assert linker_name.is_symlink()
        assert linker_name.readlink() == Path("libonnxruntime.so.1")
        runtime_license = output / "onnxruntime-LICENSE"
        runtime_notices = output / "onnxruntime-ThirdPartyNotices.txt"
        assert runtime_license.read_bytes() == LICENSE_PAYLOAD
        assert runtime_notices.read_bytes() == NOTICES_PAYLOAD
        env_lines = github_env.read_text(encoding="utf-8").splitlines()
        assert env_lines == [
            f"ORT_LIB_PATH={output}",
            f"ORT_LIB_LOCATION={output}",
            "ORT_PREFER_DYNAMIC_LINK=1",
            f"LD_LIBRARY_PATH={output}",
            "RUSTFLAGS=-C link-arg=-Wl,-rpath,$ORIGIN",
        ]
        assert github_output.read_text(encoding="utf-8").splitlines() == [
            f"runtime_library={runtime_library}",
            "runtime_entry_name=libonnxruntime.so.1",
            f"runtime_license={runtime_license}",
            f"runtime_notices={runtime_notices}",
        ]

        no_runtime_output = root / "no-runtime"
        no_runtime_github_env = root / "no-runtime-env"
        no_runtime_github_output = root / "no-runtime-output"
        result = run_preparer(
            manifest,
            "aarch64-macos",
            no_runtime_output,
            no_runtime_github_env,
            no_runtime_github_output,
        )
        assert result.returncode == 0, result.stderr
        assert not no_runtime_output.exists()
        assert not no_runtime_github_env.exists()
        assert no_runtime_github_output.read_text(encoding="utf-8").splitlines() == [
            "runtime_library=",
            "runtime_entry_name=",
            "runtime_license=",
            "runtime_notices=",
        ]

        bad_manifest = root / "bad-targets.json"
        write_manifest(bad_manifest, archive, "0" * 64)
        bad_output = root / "bad-runtime"
        result = run_preparer(
            bad_manifest,
            "aarch64-linux",
            bad_output,
            root / "bad-env",
            root / "bad-output",
        )
        assert result.returncode != 0
        assert "SHA-256 mismatch" in result.stderr
        assert not bad_output.exists()

    print("release runtime preparation tests passed")


if __name__ == "__main__":
    main()
