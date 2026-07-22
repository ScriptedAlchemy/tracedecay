"""Strict JSON schemas for frozen soak plans and executor-owned artifacts."""

from __future__ import annotations

from typing import Any

from jsonschema import Draft202012Validator

from runner_contract import ConfigError
from safe_paths import canonical_compact_json, sha256_bytes

PLAN_SCHEMA_ID = "storage-runtime-soak-plan-v2"
RESULT_SCHEMA_ID = "storage-runtime-soak-result-v2"
RECEIPT_SCHEMA_ID = "storage-runtime-soak-execution-receipt-v2"
ALLOWED_WORKLOAD_IDS = frozenset(
    {
        "storage-runtime-product-fts-v1",
        "storage-runtime-s11-product-gates-v1",
    }
)
REQUIRED_GATE_IDS = (
    "storage-runtime-maintenance-doctor-v1",
    "storage-runtime-crash-recovery-repair-v1",
    "storage-runtime-backup-restore-v1",
    "storage-runtime-logical-evidence-v1",
    "storage-runtime-resource-trends-v1",
)
RESOURCE_FIELDS = (
    "queue_depth",
    "wal_bytes",
    "readers",
    "rss_bytes",
    "fd_count",
    "cpu_seconds",
    "io_write_bytes",
)
S6_GATE_BINDINGS = {
    "storage-runtime-maintenance-doctor-v1": [
        "MaintenanceCoordinator",
        "SqliteMaintenanceDriver",
        "SqliteDoctorHealthLane",
    ],
    "storage-runtime-crash-recovery-repair-v1": [
        "MaintenanceCoordinator",
        "SqliteDoctorHealthLane",
        "SqliteCorruptionProbe",
        "SqliteRepairDriver",
        "FilesystemQuarantineStore",
    ],
    "storage-runtime-backup-restore-v1": [
        "BackupRoot",
        "FilesystemBackupStore",
        "SqliteOnlineBackupDriver",
        "RestorePublicationAuthority",
        "BackupRestoreOrchestrator",
    ],
}

SHA = {"type": "string", "pattern": "^[0-9a-f]{64}$"}
NONNEGATIVE_INTEGER = {"type": "integer", "minimum": 0}
NONNEGATIVE_NUMBER = {"type": "number", "minimum": 0}

SUSTAINED_PLAN_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": [
        "scale",
        "issue_model",
        "latency_origin",
        "offered_count",
        "rate_per_second",
        "schedule_preview",
        "preview_truncated",
    ],
    "properties": {
        "scale": {"enum": ["current", "ten_x", "overload"]},
        "issue_model": {"const": "open_loop_absolute_schedule"},
        "latency_origin": {"const": "scheduled_issue_time"},
        "offered_count": {"type": "integer", "minimum": 1},
        "rate_per_second": {"type": "number", "exclusiveMinimum": 0},
        "schedule_preview": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["request_id", "scheduled_offset_ns"],
                "properties": {
                    "request_id": NONNEGATIVE_INTEGER,
                    "scheduled_offset_ns": NONNEGATIVE_INTEGER,
                },
            },
        },
        "preview_truncated": {"type": "boolean"},
    },
}

PLAN_SCHEMA = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "additionalProperties": False,
    "required": [
        "schema",
        "workload_id",
        "gate_ids",
        "artifact_schema",
        "seed",
        "duration_seconds",
        "sample_interval_seconds",
        "operation_timeout_seconds",
        "sustained",
        "crashes",
        "crashes_truncated",
        "crash_count",
        "restores",
        "restores_truncated",
        "restore_rehearsal_count",
        "safety",
        "plan_sha256",
    ],
    "properties": {
        "schema": {"const": PLAN_SCHEMA_ID},
        "workload_id": {"enum": sorted(ALLOWED_WORKLOAD_IDS)},
        "gate_ids": {
            "type": "array",
            "prefixItems": [{"const": value} for value in REQUIRED_GATE_IDS],
            "items": False,
        },
        "artifact_schema": {"const": RESULT_SCHEMA_ID},
        "seed": NONNEGATIVE_INTEGER,
        "duration_seconds": {"type": "integer", "minimum": 1},
        "sample_interval_seconds": {"type": "number", "exclusiveMinimum": 0},
        "operation_timeout_seconds": {"type": "number", "exclusiveMinimum": 0},
        "sustained": {
            "type": "array",
            "minItems": 3,
            "maxItems": 3,
            "items": SUSTAINED_PLAN_SCHEMA,
        },
        "crashes": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["campaign_index", "scheduled_offset_ns"],
                "properties": {
                    "campaign_index": NONNEGATIVE_INTEGER,
                    "scheduled_offset_ns": NONNEGATIVE_INTEGER,
                },
            },
        },
        "crashes_truncated": {"type": "boolean"},
        "crash_count": NONNEGATIVE_INTEGER,
        "restores": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["rehearsal_index", "source", "steps"],
                "properties": {
                    "rehearsal_index": NONNEGATIVE_INTEGER,
                    "source": {"const": "explicit_frozen_fixture_copy"},
                    "steps": {
                        "const": [
                            "backup",
                            "verify_manifest",
                            "restore",
                            "logical_compare",
                        ]
                    },
                },
            },
        },
        "restores_truncated": {"type": "boolean"},
        "restore_rehearsal_count": NONNEGATIVE_INTEGER,
        "safety": {
            "type": "object",
            "additionalProperties": False,
            "required": ["profile_discovery", "live_migration", "fixture_source"],
            "properties": {
                "profile_discovery": {"const": "forbidden"},
                "live_migration": {"const": "forbidden"},
                "fixture_source": {"const": "explicit_only"},
            },
        },
        "plan_sha256": SHA,
    },
}

COUNT_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": [
        "scale",
        "offered",
        "admitted",
        "completed",
        "failed",
        "shed_runner_in_flight",
        "shed_command_saturation",
        "terminal",
        "latency_origin",
    ],
    "properties": {
        "scale": {"enum": ["current", "ten_x", "overload"]},
        "offered": NONNEGATIVE_INTEGER,
        "admitted": NONNEGATIVE_INTEGER,
        "completed": NONNEGATIVE_INTEGER,
        "failed": NONNEGATIVE_INTEGER,
        "shed_runner_in_flight": NONNEGATIVE_INTEGER,
        "shed_command_saturation": NONNEGATIVE_INTEGER,
        "terminal": NONNEGATIVE_INTEGER,
        "latency_origin": {"const": "scheduled_issue_time"},
    },
}

RECEIPT_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": [
        "schema",
        "executor_id",
        "executor_version",
        "artifact_schema",
        "status",
        "plan_sha256",
        "workload_id",
        "workload_implementation_sha256",
        "commit_sha",
        "product_binary_sha256",
        "evidence_binary_sha256",
        "environment_sha256",
        "fixture_sha256",
        "frozen_identity_sha256",
        "payload_sha256",
        "receipt_sha256",
        "coordinated_omission",
        "artifacts_bounded",
        "fixture_source",
        "fixture_schema",
        "fixture_verified",
        "product_adapter_validated",
        "product_gate_evidence",
        "product_commit_sha",
        "logical_evidence",
        "sustained",
        "crash_count_completed",
        "crash_recovery_count",
        "restore_rehearsal_count",
        "restore_verified_count",
    ],
    "properties": {
        "schema": {"const": RECEIPT_SCHEMA_ID},
        "executor_id": {"const": "tracedecay-storage-runtime-soak-executor"},
        "executor_version": {"const": 1},
        "artifact_schema": {"const": RESULT_SCHEMA_ID},
        "status": {"enum": ["completed", "failed", "timed_out"]},
        "plan_sha256": SHA,
        "workload_id": {"enum": sorted(ALLOWED_WORKLOAD_IDS)},
        "workload_implementation_sha256": SHA,
        "commit_sha": {"type": "string", "pattern": "^[0-9a-f]{40,64}$"},
        "product_binary_sha256": SHA,
        "evidence_binary_sha256": SHA,
        "environment_sha256": SHA,
        "fixture_sha256": SHA,
        "frozen_identity_sha256": SHA,
        "payload_sha256": SHA,
        "receipt_sha256": SHA,
        "coordinated_omission": {"const": False},
        "artifacts_bounded": {"const": True},
        "fixture_source": {"const": "explicit"},
        "fixture_schema": {"const": "storage-runtime-fixture-v1"},
        "fixture_verified": {"type": "boolean"},
        "product_adapter_validated": {"type": "boolean"},
        "logical_evidence": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": [
                    "schema",
                    "integrity",
                    "schema_sha256",
                    "tables",
                    "fts",
                ],
                "properties": {
                    "schema": {
                        "const": "storage-runtime-logical-sqlite-evidence-v1"
                    },
                    "integrity": {
                        "type": "object",
                        "additionalProperties": False,
                        "required": ["status", "result_sha256", "result_row_count"],
                        "properties": {
                            "status": {"const": "ok"},
                            "result_sha256": SHA,
                            "result_row_count": NONNEGATIVE_INTEGER,
                        },
                    },
                    "schema_sha256": SHA,
                    "tables": {"type": "array"},
                    "fts": {"type": "array"},
                },
            },
        },
        "product_gate_evidence": {
            "type": "array",
            "maxItems": 3,
            "items": S6_GATE_EVIDENCE_SCHEMA if "S6_GATE_EVIDENCE_SCHEMA" in globals() else {},
        },
        "product_commit_sha": {
            "oneOf": [
                {"type": "string", "pattern": "^[0-9a-f]{40,64}$"},
                {"type": "null"},
            ]
        },
        "sustained": {"type": "array", "items": COUNT_SCHEMA},
        "crash_count_completed": NONNEGATIVE_INTEGER,
        "crash_recovery_count": NONNEGATIVE_INTEGER,
        "restore_rehearsal_count": NONNEGATIVE_INTEGER,
        "restore_verified_count": NONNEGATIVE_INTEGER,
    },
}

RESOURCE_SAMPLE_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["elapsed_seconds", *RESOURCE_FIELDS],
    "properties": {
        "elapsed_seconds": NONNEGATIVE_NUMBER,
        **{name: NONNEGATIVE_NUMBER for name in RESOURCE_FIELDS},
    },
}

RESOURCE_MAP_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": list(RESOURCE_FIELDS),
    "properties": {name: NONNEGATIVE_NUMBER for name in RESOURCE_FIELDS},
}

TREND_POLICY_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": [
        "maximum_slope_per_second",
        "maximum_end_to_baseline_ratio",
        "maximum_post_eviction_ratio",
    ],
    "properties": {
        "maximum_slope_per_second": RESOURCE_MAP_SCHEMA,
        "maximum_end_to_baseline_ratio": RESOURCE_MAP_SCHEMA,
        "maximum_post_eviction_ratio": RESOURCE_MAP_SCHEMA,
        "minimum_samples": {"type": "integer", "minimum": 2},
        "maximum_samples": {"type": "integer", "minimum": 2},
        "maximum_cadence_gap_seconds": {"type": "number", "exclusiveMinimum": 0},
    },
}

RESULT_SCHEMA = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "additionalProperties": False,
    "required": [
        "schema",
        "plan_identity",
        "workload_identity",
        "commit_identity",
        "binary_identity",
        "environment_identity",
        "resource_samples",
        "post_eviction",
        "trend_policy",
        "execution_receipt",
    ],
    "properties": {
        "schema": {"const": RESULT_SCHEMA_ID},
        "plan_identity": {
            "type": "object",
            "additionalProperties": False,
            "required": ["sha256"],
            "properties": {"sha256": SHA},
        },
        "workload_identity": {
            "type": "object",
            "additionalProperties": False,
            "required": ["id", "implementation_sha256"],
            "properties": {
                "id": {"enum": sorted(ALLOWED_WORKLOAD_IDS)},
                "implementation_sha256": SHA,
            },
        },
        "commit_identity": {
            "type": "object",
            "additionalProperties": False,
            "required": ["sha"],
            "properties": {"sha": {"type": "string", "pattern": "^[0-9a-f]{40,64}$"}},
        },
        "binary_identity": {
            "type": "object",
            "additionalProperties": False,
            "required": ["product_sha256", "evidence_sha256"],
            "properties": {
                "product_sha256": SHA,
                "evidence_sha256": SHA,
            },
        },
        "environment_identity": {
            "type": "object",
            "additionalProperties": False,
            "required": ["sha256", "platform", "python", "psutil"],
            "properties": {
                "sha256": SHA,
                "platform": {"type": "string", "minLength": 1},
                "python": {"type": "string", "minLength": 1},
                "psutil": {"type": "string", "minLength": 1},
            },
        },
        "resource_samples": {"type": "array", "minItems": 2, "items": RESOURCE_SAMPLE_SCHEMA},
        "post_eviction": RESOURCE_MAP_SCHEMA,
        "trend_policy": TREND_POLICY_SCHEMA,
        "execution_receipt": RECEIPT_SCHEMA,
    },
}

PRODUCT_ADAPTER_SCHEMA = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "additionalProperties": False,
    "required": [
        "schema",
        "status",
        "evidence_status",
        "operation",
        "family",
        "product_output",
    ],
    "properties": {
        "schema": {"const": "tracedecay-storage-runtime-product-probe-v1"},
        "status": {"const": "not_evidence"},
        "evidence_status": {
            "type": "object",
            "additionalProperties": False,
            "required": ["state", "reasons"],
            "properties": {
                "state": {"const": "not_evidence"},
                "reasons": {
                    "type": "array",
                    "minItems": 1,
                    "items": {"type": "string", "minLength": 1},
                },
            },
        },
        "operation": {"const": "fts"},
        "family": {"enum": ["graph", "session"]},
        "product_output": {
            "type": "object",
            "additionalProperties": False,
            "required": ["redacted", "sha256", "byte_count"],
            "properties": {
                "redacted": {"const": True},
                "sha256": SHA,
                "byte_count": {"type": "integer", "minimum": 1},
            },
        },
    },
}

GATE_OUTCOME_SCHEMAS = {
    "storage-runtime-maintenance-doctor-v1": {
        "maintenance_reopened": {"type": "boolean"},
        "doctor_quick_check": {"enum": ["healthy", "corrupt", "not_observed"]},
        "doctor_integrity_check": {"enum": ["healthy", "corrupt", "not_observed"]},
        "writer_state": {"enum": ["ready", "faulted", "not_observed"]},
        "reader_state": {"enum": ["ready", "faulted", "not_observed"]},
        "wal_enabled": {"type": "boolean"},
    },
    "storage-runtime-crash-recovery-repair-v1": {
        "crashes_requested": NONNEGATIVE_INTEGER,
        "crashes_completed": NONNEGATIVE_INTEGER,
        "recoveries_completed": NONNEGATIVE_INTEGER,
        "doctor_detected_fault": {"type": "boolean"},
        "repair_class": {
            "enum": [
                "healthy",
                "derived_fts",
                "authoritative",
                "indeterminate",
                "not_observed",
            ]
        },
        "repair_receipt_bound": {"type": "boolean"},
        "quarantine_preserved": {"type": "boolean"},
        "recovery_health": {"enum": ["healthy", "faulted", "not_observed"]},
    },
    "storage-runtime-backup-restore-v1": {
        "restores_requested": NONNEGATIVE_INTEGER,
        "backups_completed": NONNEGATIVE_INTEGER,
        "restores_completed": NONNEGATIVE_INTEGER,
        "backup_manifest_verified": {"type": "boolean"},
        "artifact_digests_verified": {"type": "boolean"},
        "restore_verified": {"type": "boolean"},
        "replacement_published": {"type": "boolean"},
        "restored_binding_newer": {"type": "boolean"},
    },
}


def _gate_variant(gate_id: str) -> dict:
    outcome = GATE_OUTCOME_SCHEMAS[gate_id]
    return {
        "type": "object",
        "additionalProperties": False,
        "required": [
            "schema",
            "gate_id",
            "status",
            "evidence_status",
            "api_bindings",
            "fixture_sha256",
            "product_commit_sha",
            "product_binary_sha256",
            "evidence_binary_sha256",
            "logical_evidence",
            "outcome",
        ],
        "properties": {
            "schema": {"const": "storage-runtime-s6-gate-evidence-v1"},
            "gate_id": {"const": gate_id},
            "status": {"enum": ["completed", "failed", "not_run"]},
            "evidence_status": {
                "type": "object",
                "additionalProperties": False,
                "required": ["state", "reasons"],
                "properties": {
                    "state": {"enum": ["evidence", "not_evidence"]},
                    "reasons": {
                        "type": "array",
                        "items": {"type": "string", "minLength": 1},
                    },
                },
            },
            "api_bindings": {"const": S6_GATE_BINDINGS[gate_id]},
            "fixture_sha256": {"oneOf": [SHA, {"type": "null"}]},
            "product_commit_sha": {
                "oneOf": [
                    {"type": "string", "pattern": "^[0-9a-f]{40,64}$"},
                    {"type": "null"},
                ]
            },
            "product_binary_sha256": {"oneOf": [SHA, {"type": "null"}]},
            "evidence_binary_sha256": {"oneOf": [SHA, {"type": "null"}]},
            "logical_evidence": RECEIPT_SCHEMA["properties"]["logical_evidence"],
            "outcome": {
                "type": "object",
                "additionalProperties": False,
                "required": list(outcome),
                "properties": outcome,
            },
        },
    }


S6_GATE_EVIDENCE_SCHEMA = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "oneOf": [_gate_variant(gate_id) for gate_id in S6_GATE_BINDINGS],
}

S11_PRODUCT_ADAPTER_SCHEMA = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "additionalProperties": False,
    "required": ["schema", "status", "evidence_status", "operation", "gates"],
    "properties": {
        "schema": {"const": "tracedecay-storage-runtime-product-probe-v2"},
        "status": {"const": "not_evidence"},
        "evidence_status": {
            "type": "object",
            "additionalProperties": False,
            "required": ["state", "reasons"],
            "properties": {
                "state": {"const": "not_evidence"},
                "reasons": {
                    "type": "array",
                    "minItems": 1,
                    "items": {"type": "string", "minLength": 1},
                },
            },
        },
        "operation": {"const": "s11_product_gates"},
        "gates": {
            "type": "array",
            "minItems": 3,
            "maxItems": 3,
            "prefixItems": [
                _gate_variant(gate_id) for gate_id in S6_GATE_BINDINGS
            ],
            "items": False,
        },
    },
}
RECEIPT_SCHEMA["properties"]["product_gate_evidence"][
    "items"
] = S6_GATE_EVIDENCE_SCHEMA


def _validate(document: Any, schema: dict, role: str) -> None:
    errors = sorted(
        Draft202012Validator(schema).iter_errors(document),
        key=lambda error: list(error.absolute_path),
    )
    if errors:
        detail = "; ".join(
            f"{'.'.join(str(item) for item in error.absolute_path) or '$'}: {error.message}"
            for error in errors[:8]
        )
        raise ConfigError(f"{role} schema validation failed: {detail}")


def validate_plan(document: Any) -> dict:
    if isinstance(document, dict) and document.get("workload_id") not in ALLOWED_WORKLOAD_IDS:
        raise ConfigError("soak plan workload_id is not in the code allowlist")
    _validate(document, PLAN_SCHEMA, "soak plan")
    unhashed = dict(document)
    supplied = unhashed.pop("plan_sha256")
    computed = sha256_bytes(canonical_compact_json(unhashed).encode("utf-8"))
    if supplied != computed:
        raise ConfigError("soak plan schema is valid but plan_sha256 is invalid")
    return document


def validate_result(document: Any) -> dict:
    _validate(document, RESULT_SCHEMA, "soak result")
    receipt = document["execution_receipt"]
    unhashed_receipt = dict(receipt)
    supplied_receipt_hash = unhashed_receipt.pop("receipt_sha256")
    if supplied_receipt_hash != sha256_bytes(
        canonical_compact_json(unhashed_receipt).encode("utf-8")
    ):
        raise ConfigError("soak result receipt_sha256 is invalid")
    payload = dict(document)
    payload.pop("execution_receipt")
    if receipt["payload_sha256"] != sha256_bytes(
        canonical_compact_json(payload).encode("utf-8")
    ):
        raise ConfigError("soak result payload_sha256 is invalid")
    environment = dict(document["environment_identity"])
    supplied_environment_hash = environment.pop("sha256")
    if supplied_environment_hash != sha256_bytes(
        canonical_compact_json(environment).encode("utf-8")
    ):
        raise ConfigError("soak result environment identity hash is invalid")
    consistency = {
        "plan_sha256": document["plan_identity"]["sha256"],
        "workload_id": document["workload_identity"]["id"],
        "workload_implementation_sha256": document["workload_identity"][
            "implementation_sha256"
        ],
        "commit_sha": document["commit_identity"]["sha"],
        "product_binary_sha256": document["binary_identity"]["product_sha256"],
        "evidence_binary_sha256": document["binary_identity"]["evidence_sha256"],
        "environment_sha256": document["environment_identity"]["sha256"],
        "artifact_schema": document["schema"],
    }
    for key, expected in consistency.items():
        if receipt[key] != expected:
            raise ConfigError(f"soak result receipt identity mismatch: {key}")
    if (
        receipt["product_binary_sha256"]
        == receipt["evidence_binary_sha256"]
    ):
        raise ConfigError("soak result product/evidence binary identities are not distinct")
    return document


def product_adapter_output_valid(document: Any) -> bool:
    return not any(
        Draft202012Validator(PRODUCT_ADAPTER_SCHEMA).iter_errors(document)
    ) or not any(
        Draft202012Validator(S11_PRODUCT_ADAPTER_SCHEMA).iter_errors(document)
    )


def validate_s6_gate_evidence(document: Any) -> None:
    _validate(document, S6_GATE_EVIDENCE_SCHEMA, "S6 typed evidence")


def s6_gate_evidence_eligible(document: Any) -> bool:
    try:
        validate_s6_gate_evidence(document)
    except ConfigError:
        return False
    if (
        document["status"] != "completed"
        or document["evidence_status"]["state"] != "evidence"
        or document["fixture_sha256"] is None
        or document["product_commit_sha"] is None
        or document["product_binary_sha256"] is None
        or document["evidence_binary_sha256"] is None
        or document["product_binary_sha256"] == document["evidence_binary_sha256"]
        or not document["logical_evidence"]
    ):
        return False
    outcome = document["outcome"]
    if document["gate_id"] == "storage-runtime-maintenance-doctor-v1":
        return (
            outcome["maintenance_reopened"]
            and outcome["doctor_quick_check"] == "healthy"
            and outcome["doctor_integrity_check"] == "healthy"
            and outcome["writer_state"] == "ready"
            and outcome["reader_state"] == "ready"
        )
    if document["gate_id"] == "storage-runtime-crash-recovery-repair-v1":
        return (
            outcome["crashes_requested"] > 0
            and outcome["crashes_completed"] == outcome["crashes_requested"]
            and outcome["recoveries_completed"] == outcome["crashes_requested"]
            and outcome["doctor_detected_fault"]
            and (outcome["repair_receipt_bound"] or outcome["quarantine_preserved"])
            and outcome["recovery_health"] == "healthy"
        )
    return (
        outcome["restores_requested"] > 0
        and outcome["backups_completed"] == outcome["restores_requested"]
        and outcome["restores_completed"] == outcome["restores_requested"]
        and outcome["backup_manifest_verified"]
        and outcome["artifact_digests_verified"]
        and outcome["restore_verified"]
        and outcome["replacement_published"]
        and outcome["restored_binding_newer"]
    )
