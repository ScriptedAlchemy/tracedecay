"""Shared artifact identifiers, safety constants, and runner error types."""

from __future__ import annotations

import re

RUNNER_VERSION = "3.0.0"
RESULT_ARTIFACT_ID = "storage-runtime-baseline-result-v2"
IDENTITY_ARTIFACT_ID = "storage-runtime-frozen-identity-v3"
WORKLOAD_SCHEMA_VERSION = 1
RESULT_SCHEMA_VERSION = 2
IDENTITY_SCHEMA_VERSION = 3
LOGICAL_SQLITE_EVIDENCE_SCHEMA = "storage-runtime-logical-sqlite-evidence-v1"

SCRUB_ENV_PREFIXES = ("TRACEDECAY_",)
SCRUB_ENV_EXACT = ("NEXTEST_TEST_NAME",)
PROTECTED_CHILD_ENV_KEYS = {
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    "PWD",
    "OLDPWD",
    "TRACEDECAY_DATA_DIR",
    "TRACEDECAY_GLOBAL_DB",
    "TRACEDECAY_CONFIG_DIR",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SQLITE_TMPDIR",
}
SAFE_IDENTIFIER = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$")
SHA256_HEX = re.compile(r"^[0-9a-f]{64}$")
NETWORK_FILESYSTEM_TYPES = {
    "9p",
    "afs",
    "cifs",
    "ceph",
    "fuse.sshfs",
    "glusterfs",
    "nfs",
    "nfs4",
    "lustre",
    "gpfs",
    "panfs",
    "orangefs",
    "smbfs",
    "smb3",
    "sshfs",
    "davfs",
    "fuse.davfs",
    "fuse.gcsfuse",
    "fuse.rclone",
    "fuse.s3fs",
}

DEFAULT_OUTCOME_MAP = {"0": "completed"}


class RunnerError(Exception):
    """Base error for expected runner failures."""


class SafetyError(RunnerError):
    """Raised when a path or environment would touch a live profile."""


class ConfigError(RunnerError):
    """Raised for invalid or pending workload configuration."""


class ExecutionError(RunnerError):
    """Raised when a workload step fails in a way that aborts the run."""
