"""Real producer, consumer, and rollback journeys for catalog mutations."""

from __future__ import annotations

from dataclasses import dataclass
import json
from pathlib import Path
import re
import subprocess
import time
from typing import Any, Callable

from outcomes import (
    expected_state,
    fact_id_with_content,
    first_value,
    has_status,
    has_true,
    objects,
    response_handle,
)


class JourneyError(RuntimeError):
    """A negotiated mutation could not prove its complete production journey."""


Call = Callable[[str, dict[str, Any], int], dict[str, Any]]
Probe = Callable[[str, dict[str, Any], int], dict[str, Any]]
Deadline = Callable[[str], int]


@dataclass
class PreparedJourney:
    arguments: dict[str, Any]
    cleanup: Callable[[dict[str, Any]], str]
    settlement: str = "verified"
    accepted_terminal_problem: tuple[str, str] | None = None


def _fact_trust(response: dict[str, Any], fact_id: str | int) -> float | None:
    """Read the exact fact's trust score from a fact-store get response."""
    for value in objects(response):
        if value.get("fact_id") != fact_id:
            continue
        trust = value.get("trust_score")
        if isinstance(trust, (int, float)) and not isinstance(trust, bool):
            return float(trust)
        millionths = value.get("trust_score_millionths")
        if isinstance(millionths, int) and not isinstance(millionths, bool):
            return millionths / 1_000_000
    return None


def _seeded_fact(
    call: Call,
    deadline: Deadline,
    content: str,
    *,
    entities: list[str] | None = None,
    trust: float = 0.5,
) -> str | int:
    """Produce one real isolated fact and return its structured identity."""
    added = call(
        "tracedecay_fact_store_add",
        {
            "content": content,
            "category": "tool",
            "trust": trust,
            "source_label": "catalog_sweep",
            "entities": entities or [],
            "format": "json",
        },
        deadline("tracedecay_fact_store_add"),
    )
    fact_id = fact_id_with_content(added, content)
    if fact_id is None:
        raise JourneyError("fact producer omitted its structured fact identity")
    return fact_id


def _fact_commit(
    response: dict[str, Any], fact_id: str | int, dispositions: set[str]
) -> dict[str, Any]:
    for value in objects(response):
        if value.get("fact_id") != fact_id or value.get("disposition") not in dispositions:
            continue
        event_id = value.get("last_event_id")
        event_ids = value.get("committed_event_ids")
        if (
            isinstance(event_id, str)
            and event_id
            and isinstance(event_ids, list)
            and event_id in event_ids
        ):
            return value
    raise JourneyError("fact mutation omitted its exact retained commit receipt")


def _fact_outcome(
    response: dict[str, Any], outcome: str, fact_id: str | int
) -> dict[str, Any]:
    for value in objects(response):
        commit = value.get("commit")
        if (
            value.get("outcome") == outcome
            and isinstance(commit, dict)
            and commit.get("fact_id") == fact_id
        ):
            return value
    raise JourneyError(f"fact mutation omitted its {outcome} outcome")


def _fact_unavailable(response: dict[str, Any], fact_id: str | int) -> bool:
    return any(
        value.get("fact_id") == fact_id and value.get("payload_access") == "deleted"
        for value in objects(response)
    )


def _remove_seeded_fact(call: Call, deadline: Deadline, fact_id: str | int) -> str:
    removed = call(
        "tracedecay_fact_store_remove",
        {"fact_id": fact_id, "format": "json"},
        deadline("tracedecay_fact_store_remove"),
    )
    _fact_outcome(removed, "removed", fact_id)
    commit = _fact_commit(removed, fact_id, {"committed", "idempotent_replay"})
    fetched = call(
        "tracedecay_fact_store_get",
        {"fact_id": fact_id, "format": "json"},
        deadline("tracedecay_fact_store_get"),
    )
    if not _fact_unavailable(fetched, fact_id):
        raise JourneyError("fact removal did not preserve its exact deleted tombstone")
    return commit["last_event_id"]


FACT_READ_TOOLS = frozenset(
    {
        "tracedecay_fact_store_get",
        "tracedecay_fact_store_list",
        "tracedecay_fact_store_probe",
        "tracedecay_fact_store_reason",
        "tracedecay_fact_store_related",
        "tracedecay_fact_store_search",
    }
)


def prime_fact_read_lifecycle(
    fixture: dict[str, Any], call: Call, deadline: Deadline
) -> None:
    """Seed one connected fact pair for the public fact read surfaces."""
    suffix = str(time.monotonic_ns())
    alpha = f"tool-sweep-alpha-{suffix}"
    beta = f"tool-sweep-beta-{suffix}"
    gamma = f"tool-sweep-gamma-{suffix}"
    content = f"catalog sweep constellation {suffix} alpha beta"
    related_content = f"catalog sweep constellation {suffix} beta gamma"
    fact_id = _seeded_fact(
        call, deadline, content, entities=[alpha, beta], trust=0.8
    )
    related_fact_id = _seeded_fact(
        call, deadline, related_content, entities=[beta, gamma], trust=0.7
    )
    fixture.update(
        {
            "fact_id": fact_id,
            "fact_content": content,
            "fact_related_id": related_fact_id,
            "fact_related_content": related_content,
            "fact_read_arguments": {
                "tracedecay_fact_store_get": {"fact_id": fact_id, "format": "json"},
                "tracedecay_fact_store_list": {
                    "category": "tool", "limit": 200, "format": "json"
                },
                "tracedecay_fact_store_search": {
                    "query": f"constellation {suffix} alpha",
                    "limit": 20,
                    "format": "json",
                },
                "tracedecay_fact_store_probe": {
                    "entity": alpha, "limit": 20, "format": "json"
                },
                "tracedecay_fact_store_related": {
                    "entity": alpha, "limit": 20, "format": "json"
                },
                "tracedecay_fact_store_reason": {
                    "entities": [alpha, beta], "limit": 20, "format": "json"
                },
            },
        }
    )


def validate_fact_read_response(
    name: str, response: dict[str, Any], fixture: dict[str, Any]
) -> None:
    """Require each fact read to consume the exact sweep-produced graph."""
    if name not in FACT_READ_TOOLS:
        return
    fact_id = fixture.get("fact_id")
    content = fixture.get("fact_content")
    if not isinstance(content, str) or not content or not isinstance(fact_id, (str, int)):
        raise JourneyError(f"{name} has no sweep-produced fact input")
    if fact_id_with_content(response, content) != fact_id:
        raise JourneyError(f"{name} did not return the sweep-produced fact")
    if name == "tracedecay_fact_store_related":
        related_content = fixture.get("fact_related_content")
        if not isinstance(related_content, str) or fact_id_with_content(
            response, related_content
        ) != fixture.get("fact_related_id"):
            raise JourneyError("fact related did not traverse the seeded shared entity")


def _object_field(response: dict[str, Any], name: str) -> dict[str, Any]:
    for value in objects(response):
        candidate = value.get(name)
        if isinstance(candidate, dict):
            return candidate
    raise JourneyError(f"producer omitted its structured {name}")


_SHA256 = re.compile(r"sha256:[0-9a-f]{64}")
WORKFLOW_LIFECYCLE_EFFECTS = frozenset(
    {
        "tracedecay_workflow_register_definition",
        "tracedecay_workflow_activate_definition",
        "tracedecay_workflow_retire_definition",
        "tracedecay_workflow_reject_definition",
        "tracedecay_workflow_handoff_issue",
        "tracedecay_workflow_handoff_redeem",
        "tracedecay_workflow_start_run",
        "tracedecay_workflow_pause_run",
        "tracedecay_workflow_resume_run",
        "tracedecay_workflow_cancel_run",
    }
)
_WORK_LIFECYCLE_EFFECTS = frozenset(
    {
        "tracedecay_work_create",
        "tracedecay_work_review_proposal",
        "tracedecay_work_accept_proposal",
        "tracedecay_work_admit_execution",
        "tracedecay_work_start_attempt",
        "tracedecay_work_synthesize",
        "tracedecay_work_cancel_attempt",
        "tracedecay_work_resume_attempts",
        "tracedecay_work_retry_attempt",
        "tracedecay_work_mutate_graph",
        "tracedecay_work_adjudicate_duplicate",
        "tracedecay_work_adjudicate_leak",
        "tracedecay_work_pause_run",
        "tracedecay_work_resume_run",
        "tracedecay_work_admit_placement",
        "tracedecay_work_release_placement",
    }
)


def _manifest_digest(value: Any, field: str) -> str:
    if not isinstance(value, str) or _SHA256.fullmatch(value) is None:
        raise JourneyError(f"producer omitted its canonical {field}")
    return value


def _workflow_policy_digest(response: dict[str, Any]) -> str:
    for value in objects(response):
        policy = value.get("policy")
        if isinstance(policy, dict) and "digest" in policy:
            return _manifest_digest(policy["digest"], "Workflow policy digest")
    raise JourneyError("configuration evidence omitted its Workflow policy digest")


def _workflow_catalog_digest(response: dict[str, Any]) -> str:
    for value in objects(response):
        diagnostic = value.get("diagnostic")
        if not isinstance(diagnostic, dict):
            continue
        if diagnostic.get("code") != "workflow.catalog.pin_mismatch":
            continue
        message = diagnostic.get("message")
        if not isinstance(message, str):
            break
        match = re.search(
            r"pinned_catalog_digest expected (sha256:[0-9a-f]{64}), observed ",
            message,
        )
        if match is not None:
            return match.group(1)
    raise JourneyError("Workflow catalog probe omitted its current digest diagnostic")


def _workflow_definition(
    *,
    definition_id: str,
    version: int,
    project_id: str,
    policy_digest: str,
    configuration_digest: str,
    catalog_digest: str,
    changed: bool = False,
) -> dict[str, Any]:
    return {
        "definition_id": definition_id,
        "definition_version": version,
        "project_id": project_id,
        "steps": [
            {
                "step_id": "step.tool-sweep.inspect",
                "operation": "operation.work.start_attempt",
                "predecessors": [],
                "inputs": [],
                "outputs": ["inspection"] if changed else [],
                "fan_out": None,
            }
        ],
        "pinned_policy_digest": policy_digest,
        "pinned_configuration_digest": configuration_digest,
        "pinned_catalog_digest": catalog_digest,
    }


def _exact_workflow_definition(
    response: dict[str, Any], definition_id: str, version: int,
) -> dict[str, Any]:
    for value in objects(response):
        if (
            value.get("definition_id") == definition_id
            and value.get("definition_version") == version
            and isinstance(value.get("steps"), list)
        ):
            return value
    raise JourneyError("Workflow definition consumer omitted the produced identity")


def _workflow_disposition(
    response: dict[str, Any], definition_id: str, state: str,
) -> dict[str, Any]:
    for value in objects(response):
        if (
            value.get("definition_id") == definition_id
            and value.get("state") == state
            and isinstance(value.get("revision"), int)
        ):
            return value
    raise JourneyError(f"Workflow transition omitted its {state} disposition")


def _workflow_run(
    response: dict[str, Any], run_id: str, statuses: set[str],
) -> dict[str, Any]:
    for value in objects(response):
        if (
            value.get("run_id") == run_id
            and value.get("status") in statuses
            and isinstance(value.get("sequence"), int)
        ):
            return value
    expected = ", ".join(sorted(statuses))
    raise JourneyError(f"Workflow run omitted {run_id} in status {expected}")


def _workflow_effect_authority(response: dict[str, Any]) -> tuple[str, dict[str, Any]]:
    for value in objects(response):
        actor = value.get("actor")
        scope = value.get("scope")
        if (
            isinstance(actor, str)
            and isinstance(scope, dict)
            and all(
                isinstance(scope.get(field), str)
                for field in ("project_id", "repository_id", "worktree_id")
            )
        ):
            return actor, scope
    raise JourneyError("Workflow effect omitted its admitted actor and scope")


def _handoff_grant(response: dict[str, Any], scope: dict[str, Any]) -> dict[str, Any]:
    for value in objects(response):
        if (
            value.get("scope") == scope
            and isinstance(value.get("token_digest"), str)
            and isinstance(value.get("frontier_digest"), str)
            and isinstance(value.get("frontier"), dict)
        ):
            return value
    raise JourneyError("handoff issue omitted the exact grant")


def _handoff_redeemed(
    response: dict[str, Any], scope: dict[str, Any], frontier_digest: str,
) -> dict[str, Any]:
    for value in objects(response):
        if (
            value.get("scope") == scope
            and value.get("frontier_digest") == frontier_digest
            and isinstance(value.get("frontier"), dict)
            and isinstance(value.get("redeemed_at"), int)
        ):
            return value
    raise JourneyError("handoff redeem omitted the issued grant frontier")


def _prepare_workflow_effect_journey(
    name: str,
    fixture: dict[str, Any],
    call: Call,
    deadline: Deadline,
    policy_digest: str,
    configuration_digest: str,
    catalog_digest: str,
) -> PreparedJourney:
    retained = fixture["workflow_effect_arguments"]
    definition_id = fixture["workflow_definition_id"]
    if name in {
        "tracedecay_workflow_handoff_issue",
        "tracedecay_workflow_handoff_redeem",
    }:
        suffix = str(time.monotonic_ns())
        actor = fixture["workflow_actor"]
        admitted_scope = fixture["workflow_scope"]
        task_id = f"task.tool-sweep.workflow.{suffix}"
        scope = {
            "project_id": admitted_scope["project_id"],
            "repository_id": admitted_scope["repository_id"],
            "worktree_id": admitted_scope["worktree_id"],
            "definition_id": definition_id,
            "definition_version": 1,
            "step_id": fixture["workflow_definition_v1"]["steps"][0]["step_id"],
            "task_id": task_id,
            "thread_id": f"thread.tool-sweep.{suffix}",
            "run_id": fixture["workflow_run_id"],
            "from_actor_id": actor,
            "to_actor_id": actor,
        }
        frontier = {
            "task_id": task_id,
            "work_version": 1,
            "attempts": [],
            "unknowns": [],
            "blockers": [],
            "legal_actions": ["Inspect the redeemed disposable frontier."],
            "lineage": {
                "issued_by": actor,
                "issued_at": int(time.time() * 1_000_000),
                "prior_frontier_digest": None,
            },
        }
        secret = f"tool-sweep-handoff-{suffix}-" + "s" * 32
        issue_arguments = {
            "scope": scope,
            "secret": secret,
            "frontier": frontier,
            "format": "json",
        }
        redeem_arguments = {
            "secret": secret,
            "expected_scope": scope,
            "format": "json",
        }
        issued: dict[str, Any] | None = None
        if name == "tracedecay_workflow_handoff_redeem":
            issued = _handoff_grant(
                call(
                    "tracedecay_workflow_handoff_issue",
                    issue_arguments,
                    deadline("tracedecay_workflow_handoff_issue"),
                ),
                scope,
            )

        def cleanup(response: dict[str, Any]) -> str:
            if name == "tracedecay_workflow_handoff_issue":
                grant = _handoff_grant(response, scope)
                replayed = _handoff_grant(
                    call(
                        "tracedecay_workflow_handoff_issue",
                        issue_arguments,
                        deadline("tracedecay_workflow_handoff_issue"),
                    ),
                    scope,
                )
                if replayed["token_digest"] != grant["token_digest"]:
                    raise JourneyError("handoff issue replay changed the grant identity")
                redeemed = call(
                    "tracedecay_workflow_handoff_redeem",
                    redeem_arguments,
                    deadline("tracedecay_workflow_handoff_redeem"),
                )
                _handoff_redeemed(redeemed, scope, grant["frontier_digest"])
                return "issued grant replayed exactly, then consumed with its scope and frontier"
            assert issued is not None
            _handoff_redeemed(response, scope, issued["frontier_digest"])
            return "issued grant redeemed once in the disposable store"

        return PreparedJourney(
            issue_arguments if name == "tracedecay_workflow_handoff_issue" else redeem_arguments,
            cleanup,
            "contained",
        )
    if name in {
        "tracedecay_workflow_register_definition",
        "tracedecay_workflow_activate_definition",
        "tracedecay_workflow_retire_definition",
        "tracedecay_workflow_reject_definition",
    }:
        arguments = retained[name]

        def cleanup(response: dict[str, Any]) -> str:
            if name == "tracedecay_workflow_register_definition":
                _exact_workflow_definition(response, definition_id, 1)
            else:
                state = {
                    "tracedecay_workflow_activate_definition": "active",
                    "tracedecay_workflow_retire_definition": "retired",
                    "tracedecay_workflow_reject_definition": "rejected",
                }[name]
                _workflow_disposition(response, definition_id, state)
            return "exact journal replay verified; terminal definition stays in disposable store"

        return PreparedJourney(dict(arguments), cleanup, "contained")

    if name not in {
        "tracedecay_workflow_start_run",
        "tracedecay_workflow_pause_run",
        "tracedecay_workflow_resume_run",
        "tracedecay_workflow_cancel_run",
    }:
        raise JourneyError(f"no Workflow lifecycle journey for {name}")

    suffix = str(time.monotonic_ns())
    effect_definition_id = f"workflow.tool-sweep.effect.{suffix}"
    definition = _workflow_definition(
        definition_id=effect_definition_id,
        version=1,
        project_id=fixture["project_id"],
        policy_digest=policy_digest,
        configuration_digest=configuration_digest,
        catalog_digest=catalog_digest,
    )
    call(
        "tracedecay_workflow_register_definition",
        {"definition": definition, "format": "json"},
        deadline("tracedecay_workflow_register_definition"),
    )
    activated = call(
        "tracedecay_workflow_activate_definition",
        {
            "definition_id": effect_definition_id,
            "definition_version": 1,
            "expected_revision": 1,
            "format": "json",
        },
        deadline("tracedecay_workflow_activate_definition"),
    )
    active = _workflow_disposition(activated, effect_definition_id, "active")
    run_id = f"workflow-run.tool-sweep.effect.{suffix}"
    start = {
        "run_id": run_id,
        "definition_id": effect_definition_id,
        "definition_version": 1,
        "provider": {
            "route": {
                "provider_id": "provider.work.codex-cli",
                "route_id": "route.tool-sweep.workflow",
            },
            "backend": "codex_cli",
            "model": "tool-sweep",
            "priority": 1,
        },
        "fan_out": None,
        "command_id": f"command.workflow.effect.start.{suffix}",
        "format": "json",
    }
    started: dict[str, Any] | None = None
    paused: dict[str, Any] | None = None
    if name != "tracedecay_workflow_start_run":
        started = _workflow_run(
            call(
                "tracedecay_workflow_start_run",
                start,
                deadline("tracedecay_workflow_start_run"),
            ),
            run_id,
            {"running"},
        )
    if name == "tracedecay_workflow_resume_run":
        assert started is not None
        paused = _workflow_run(
            call(
                "tracedecay_workflow_pause_run",
                {
                    "run_id": run_id,
                    "expected_sequence": started["sequence"],
                    "command_id": f"command.workflow.effect.pause.{suffix}",
                    "format": "json",
                },
                deadline("tracedecay_workflow_pause_run"),
            ),
            run_id,
            {"paused"},
        )
    if name == "tracedecay_workflow_start_run":
        arguments = start
    elif name == "tracedecay_workflow_pause_run":
        assert started is not None
        arguments = {
            "run_id": run_id,
            "expected_sequence": started["sequence"],
            "command_id": f"command.workflow.effect.pause.{suffix}",
            "format": "json",
        }
    elif name == "tracedecay_workflow_resume_run":
        assert paused is not None
        arguments = {
            "run_id": run_id,
            "expected_sequence": paused["sequence"],
            "command_id": f"command.workflow.effect.resume.{suffix}",
            "format": "json",
        }
    else:
        assert started is not None
        arguments = {
            "run_id": run_id,
            "expected_sequence": started["sequence"],
            "command_id": f"command.workflow.effect.cancel.{suffix}",
            "format": "json",
        }

    def cleanup(response: dict[str, Any]) -> str:
        projection = _workflow_run(
            response,
            run_id,
            {
                "running" if name in {"tracedecay_workflow_start_run", "tracedecay_workflow_resume_run"} else
                "paused" if name == "tracedecay_workflow_pause_run" else "cancelled"
            },
        )
        if projection["status"] == "paused":
            projection = _workflow_run(
                call(
                    "tracedecay_workflow_resume_run",
                    {
                        "run_id": run_id,
                        "expected_sequence": projection["sequence"],
                        "command_id": f"command.workflow.effect.cleanup.resume.{suffix}",
                        "format": "json",
                    },
                    deadline("tracedecay_workflow_resume_run"),
                ),
                run_id,
                {"running"},
            )
        if projection["status"] == "running":
            projection = _workflow_run(
                call(
                    "tracedecay_workflow_cancel_run",
                    {
                        "run_id": run_id,
                        "expected_sequence": projection["sequence"],
                        "command_id": f"command.workflow.effect.cleanup.cancel.{suffix}",
                        "format": "json",
                    },
                    deadline("tracedecay_workflow_cancel_run"),
                ),
                run_id,
                {"cancelled"},
            )
        observed = call(
            "tracedecay_workflow_get_run",
            {"run_id": run_id, "format": "json"},
            deadline("tracedecay_workflow_get_run"),
        )
        _workflow_run(observed, run_id, {"cancelled"})
        retired = call(
            "tracedecay_workflow_retire_definition",
            {
                "definition_id": effect_definition_id,
                "definition_version": 1,
                "expected_revision": active["revision"],
                "format": "json",
            },
            deadline("tracedecay_workflow_retire_definition"),
        )
        _workflow_disposition(retired, effect_definition_id, "retired")
        return "run reached cancelled and definition retired in disposable store"

    return PreparedJourney(arguments, cleanup, "contained")


def prime_workflow_lifecycle(
    fixture: dict[str, Any],
    call: Call,
    probe: Probe,
    deadline: Deadline,
    effect_target: str | None = None,
) -> None:
    """Exercise one pinned definition and contained no-fan-out run lifecycle."""
    suffix = str(time.monotonic_ns())
    project_id = fixture["project_id"]
    configuration = call(
        "tracedecay_configuration_get",
        {"key": fixture["configuration_key"], "format": "json"},
        deadline("tracedecay_configuration_get"),
    )
    policy_digest = _workflow_policy_digest(configuration)
    configuration_digest = _manifest_digest(
        first_value(configuration, {"effective_behavior_digest"}),
        "Workflow configuration digest",
    )
    definition_id = f"workflow.tool-sweep.{suffix}"
    stale_definition = _workflow_definition(
        definition_id=definition_id,
        version=1,
        project_id=project_id,
        policy_digest=policy_digest,
        configuration_digest=configuration_digest,
        catalog_digest="sha256:" + "0" * 64,
    )
    catalog_probe = probe(
        "tracedecay_workflow_validate_definition",
        {"definition": stale_definition, "format": "json"},
        deadline("tracedecay_workflow_validate_definition"),
    )
    catalog_digest = _workflow_catalog_digest(catalog_probe)
    definition_v1 = {
        **stale_definition,
        "pinned_catalog_digest": catalog_digest,
    }
    definition_v2 = _workflow_definition(
        definition_id=definition_id,
        version=2,
        project_id=project_id,
        policy_digest=policy_digest,
        configuration_digest=configuration_digest,
        catalog_digest=catalog_digest,
        changed=True,
    )
    validated = call(
        "tracedecay_workflow_validate_definition",
        {"definition": definition_v1, "format": "json"},
        deadline("tracedecay_workflow_validate_definition"),
    )
    _exact_workflow_definition(validated, definition_id, 1)
    for definition in (definition_v1, definition_v2):
        registered = call(
            "tracedecay_workflow_register_definition",
            {"definition": definition, "format": "json"},
            deadline("tracedecay_workflow_register_definition"),
        )
        _exact_workflow_definition(
            registered, definition_id, definition["definition_version"]
        )
    fetched = call(
        "tracedecay_workflow_get_definition",
        {"definition_id": definition_id, "definition_version": 1, "format": "json"},
        deadline("tracedecay_workflow_get_definition"),
    )
    _exact_workflow_definition(fetched, definition_id, 1)
    listed = call(
        "tracedecay_workflow_list_definitions",
        {"format": "json"},
        deadline("tracedecay_workflow_list_definitions"),
    )
    _exact_workflow_definition(listed, definition_id, 1)
    history = call(
        "tracedecay_workflow_definition_history",
        {"definition_id": definition_id, "format": "json"},
        deadline("tracedecay_workflow_definition_history"),
    )
    _exact_workflow_definition(history, definition_id, 1)
    _exact_workflow_definition(history, definition_id, 2)
    diff = call(
        "tracedecay_workflow_diff_definition",
        {
            "definition_id": definition_id,
            "from_version": 1,
            "to_version": 2,
            "format": "json",
        },
        deadline("tracedecay_workflow_diff_definition"),
    )
    if not any(
        value.get("definition_id") == definition_id
        and value.get("from_version") == 1
        and value.get("to_version") == 2
        and "step.tool-sweep.inspect" in value.get("changed_steps", [])
        for value in objects(diff)
    ):
        raise JourneyError("Workflow diff did not identify the changed produced step")
    activated = call(
        "tracedecay_workflow_activate_definition",
        {
            "definition_id": definition_id,
            "definition_version": 1,
            "expected_revision": 1,
            "format": "json",
        },
        deadline("tracedecay_workflow_activate_definition"),
    )
    active = _workflow_disposition(activated, definition_id, "active")
    workflow_actor, workflow_scope = _workflow_effect_authority(activated)

    provider = {
        "route": {
            "provider_id": "provider.work.codex-cli",
            "route_id": "route.tool-sweep.workflow",
        },
        "backend": "codex_cli",
        "model": "tool-sweep",
        "priority": 1,
    }
    run_id = f"workflow-run.tool-sweep.{suffix}"
    start_arguments = {
        "run_id": run_id,
        "definition_id": definition_id,
        "definition_version": 1,
        "provider": provider,
        "fan_out": None,
        "command_id": f"command.workflow.start.{suffix}",
        "format": "json",
    }
    started = call(
        "tracedecay_workflow_start_run",
        start_arguments,
        deadline("tracedecay_workflow_start_run"),
    )
    running = _workflow_run(started, run_id, {"running"})
    observed = call(
        "tracedecay_workflow_get_run",
        {"run_id": run_id, "format": "json"},
        deadline("tracedecay_workflow_get_run"),
    )
    _workflow_run(observed, run_id, {"running"})
    pause_arguments = {
        "run_id": run_id,
        "expected_sequence": running["sequence"],
        "command_id": f"command.workflow.pause.{suffix}",
        "format": "json",
    }
    paused = call(
        "tracedecay_workflow_pause_run",
        pause_arguments,
        deadline("tracedecay_workflow_pause_run"),
    )
    paused_run = _workflow_run(paused, run_id, {"paused"})
    resume_arguments = {
        "run_id": run_id,
        "expected_sequence": paused_run["sequence"],
        "command_id": f"command.workflow.resume.{suffix}",
        "format": "json",
    }
    resumed = call(
        "tracedecay_workflow_resume_run",
        resume_arguments,
        deadline("tracedecay_workflow_resume_run"),
    )
    resumed_run = _workflow_run(resumed, run_id, {"running"})
    cancel_arguments = {
        "run_id": run_id,
        "expected_sequence": resumed_run["sequence"],
        "command_id": f"command.workflow.cancel.{suffix}",
        "format": "json",
    }
    cancelled = call(
        "tracedecay_workflow_cancel_run",
        cancel_arguments,
        deadline("tracedecay_workflow_cancel_run"),
    )
    _workflow_run(cancelled, run_id, {"cancelled"})
    retired_arguments = {
        "definition_id": definition_id,
        "definition_version": 1,
        "expected_revision": active["revision"],
        "format": "json",
    }
    retired = call(
        "tracedecay_workflow_retire_definition",
        retired_arguments,
        deadline("tracedecay_workflow_retire_definition"),
    )
    _workflow_disposition(retired, definition_id, "retired")
    rejected = call(
        "tracedecay_workflow_reject_definition",
        {
            "definition_id": definition_id,
            "definition_version": 2,
            "expected_revision": 1,
            "format": "json",
        },
        deadline("tracedecay_workflow_reject_definition"),
    )
    _workflow_disposition(rejected, definition_id, "rejected")

    fixture.update(
        {
            "workflow_definition_id": definition_id,
            "workflow_definition_v1": definition_v1,
            "workflow_run_id": run_id,
            "workflow_actor": workflow_actor,
            "workflow_scope": workflow_scope,
            "workflow_effect_arguments": {
                "tracedecay_workflow_register_definition": {
                    "definition": definition_v1,
                    "format": "json",
                },
                "tracedecay_workflow_activate_definition": {
                    "definition_id": definition_id,
                    "definition_version": 1,
                    "expected_revision": 1,
                    "format": "json",
                },
                "tracedecay_workflow_retire_definition": retired_arguments,
                "tracedecay_workflow_reject_definition": {
                    "definition_id": definition_id,
                    "definition_version": 2,
                    "expected_revision": 1,
                    "format": "json",
                },
            },
            "workflow_read_arguments": {
                "tracedecay_workflow_validate_definition": {
                    "definition": definition_v1,
                    "format": "json",
                },
                "tracedecay_workflow_get_definition": {
                    "definition_id": definition_id,
                    "definition_version": 1,
                    "format": "json",
                },
                "tracedecay_workflow_list_definitions": {"format": "json"},
                "tracedecay_workflow_definition_history": {
                    "definition_id": definition_id,
                    "format": "json",
                },
                "tracedecay_workflow_diff_definition": {
                    "definition_id": definition_id,
                    "from_version": 1,
                    "to_version": 2,
                    "format": "json",
                },
                "tracedecay_workflow_get_run": {"run_id": run_id, "format": "json"},
            },
        }
    )
    if effect_target in WORKFLOW_LIFECYCLE_EFFECTS:
        fixture["workflow_effect_journey"] = _prepare_workflow_effect_journey(
            effect_target,
            fixture,
            call,
            deadline,
            policy_digest,
            configuration_digest,
            catalog_digest,
        )


def prime_work_lifecycle(
    fixture: dict[str, Any], call: Call, deadline: Deadline,
    effect_target: str | None = None,
) -> None:
    """Create, admit, start, inspect, and contain one real disposable Work task."""
    suffix = f"{time.monotonic_ns()}"
    occurred_at = int(time.time() * 1_000_000)
    selection = {
        "selection": "relations",
        "relation_scopes": [
            {
                "kind": "repository",
                "project_id": fixture["project_id"],
                "repository_id": fixture["repository_id"],
            }
        ],
    }
    initiative_id = f"initiative.tool-sweep.{suffix}"
    plan_id = f"plan.tool-sweep.{suffix}"
    milestone_id = f"milestone.tool-sweep.{suffix}"
    task_id = f"task.tool-sweep.{suffix}"
    proposal_id = f"proposal.tool-sweep.{suffix}"
    run_id = f"run.tool-sweep.{suffix}"
    attempt_id = f"attempt.tool-sweep.{suffix}"
    prepare_create = {
        "selection": selection,
        "change": {
            "change": "create_task",
            "initiative": {
                "id": initiative_id,
                "title": "Tool sweep initiative",
                "created_at": occurred_at,
            },
            "plan": {
                "id": plan_id,
                "initiative_id": initiative_id,
                "title": "Tool sweep plan",
                "created_at": occurred_at,
            },
            "milestone": {
                "id": milestone_id,
                "plan_id": plan_id,
                "title": "Tool sweep milestone",
                "created_at": occurred_at,
            },
            "item": {
                "input": {
                    "task_id": task_id,
                    "hierarchy": {
                        "initiative_id": initiative_id,
                        "plan_id": plan_id,
                        "milestone_id": milestone_id,
                    },
                    "title": "Prove the public Work lifecycle",
                    "dependencies": [],
                    "informational_relations": [],
                    "causal_candidates": [],
                    "acceptance_criteria": [],
                    "effort": 1,
                    "scheduled_at": None,
                    "deadline": None,
                    "created_at": occurred_at,
                    "updated_at": occurred_at,
                },
                "accepted_proposal": None,
                "accepted_route": None,
                "execution_admitted_at": None,
                "accepted_attempts": [],
                "accepted_criteria": {},
                "accepted_at": None,
                "archived_at": None,
                "evidence_links": [],
                "handoffs": [],
            },
        },
        "evidence": [],
        "format": "json",
    }
    prepared_create = call(
        "tracedecay_work_prepare_graph_mutation",
        prepare_create,
        deadline("tracedecay_work_prepare_graph_mutation"),
    )
    create_request = _object_field(prepared_create, "request")
    created = call(
        "tracedecay_work_create", create_request, deadline("tracedecay_work_create")
    )
    if has_true(created, "replayed"):
        raise JourneyError("fresh Work create unexpectedly replayed")

    generate_arguments = {
        "selection": selection,
        "task_id": task_id,
        "proposal_id": proposal_id,
        # Current graph reads are bounded by this observation instant. Read
        # after the create call has returned so its committed publication is
        # inside the requested temporal window.
        "occurred_at": int(time.time() * 1_000_000),
        "format": "json",
    }
    generated = call(
        "tracedecay_work_generate_proposal",
        generate_arguments,
        deadline("tracedecay_work_generate_proposal"),
    )
    proposal = _object_field(generated, "proposal")
    initial_version = _object_field(generated, "verified_graph_version")
    prepared_accept = call(
        "tracedecay_work_prepare_graph_mutation",
        {
            "selection": selection,
            "change": {
                "change": "decide_proposal",
                "proposal": proposal,
                "disposition": "accepted",
            },
            "evidence": [],
            "format": "json",
        },
        deadline("tracedecay_work_prepare_graph_mutation"),
    )
    accept_request = _object_field(prepared_accept, "request")
    accepted = call(
        "tracedecay_work_accept_proposal",
        accept_request,
        deadline("tracedecay_work_accept_proposal"),
    )
    accepted_version = _object_field(accepted, "verified_graph_version")
    prepared_admit = call(
        "tracedecay_work_prepare_graph_mutation",
        {
            "selection": selection,
            "change": {
                "change": "admit_execution",
                "task_id": task_id,
                "based_on_version": accepted_version["graph_version"],
            },
            "evidence": [],
            "format": "json",
        },
        deadline("tracedecay_work_prepare_graph_mutation"),
    )
    admit_request = _object_field(prepared_admit, "request")
    admitted = call(
        "tracedecay_work_admit_execution",
        admit_request,
        deadline("tracedecay_work_admit_execution"),
    )
    execution_snapshot = _object_field(admitted, "execution_snapshot")
    admitted_mutation = _object_field(admitted, "mutation")
    admitted_version = _object_field(admitted_mutation, "verified_graph_version")

    placement_arguments = {
        "task_id": task_id,
        "run_id": run_id,
        "target": {
            "kind": "no_managed_placement",
            "root": None,
            "network_free": True,
            "in_place_acknowledged": False,
        },
        "occurred_at": int(time.time() * 1_000_000),
        "format": "json",
    }
    call(
        "tracedecay_work_placement_preflight",
        placement_arguments,
        deadline("tracedecay_work_placement_preflight"),
    )
    admitted_placement = call(
        "tracedecay_work_admit_placement",
        placement_arguments,
        deadline("tracedecay_work_admit_placement"),
    )
    start_arguments = {
        "task_id": task_id,
        "run_id": run_id,
        "attempt_id": attempt_id,
        "operation": "operation.work.start_attempt",
        "execution_snapshot": execution_snapshot,
        "worktree_root": fixture["root"],
        "reference": None,
        "commit": fixture["commit"],
        "instructions": "Inspect the disposable fixture only.",
        "effect_state": "observational",
        "occurred_at": int(time.time() * 1_000_000),
        "format": "json",
    }
    started = call(
        "tracedecay_work_start_attempt",
        start_arguments,
        deadline("tracedecay_work_start_attempt"),
    )
    started_identity = _object_field(started, "identity")
    if started_identity.get("attempt_id") != attempt_id:
        raise JourneyError("Work start returned a different attempt identity")
    status_arguments = {
        "task_id": task_id,
        "run_id": run_id,
        "attempt_id": attempt_id,
        "format": "json",
    }
    status = call(
        "tracedecay_work_attempt_status",
        status_arguments,
        deadline("tracedecay_work_attempt_status"),
    )
    if _object_field(status, "identity").get("attempt_id") != attempt_id:
        raise JourneyError("Work status did not consume the started attempt identity")
    cancel_arguments = {
        **status_arguments,
        "request_id": f"cancel.tool-sweep.{suffix}",
        "occurred_at": int(time.time() * 1_000_000),
    }
    cancelled = call(
        "tracedecay_work_cancel_attempt",
        cancel_arguments,
        deadline("tracedecay_work_cancel_attempt"),
    )
    if _object_field(cancelled, "identity").get("attempt_id") != attempt_id:
        raise JourneyError("Work cancellation did not retain the attempt identity")
    cancelled_attempt = next(
        (
            value
            for value in objects(cancelled)
            if value.get("identity") == started_identity
            and isinstance(value.get("state"), str)
        ),
        None,
    )

    duplicate_attempt_id = f"attempt.duplicate-probe.tool-sweep.{suffix}"
    duplicate_start = {
        **start_arguments,
        "attempt_id": duplicate_attempt_id,
        "occurred_at": int(time.time() * 1_000_000),
    }
    duplicate_started = call(
        "tracedecay_work_start_attempt",
        duplicate_start,
        deadline("tracedecay_work_start_attempt"),
    )
    duplicate_identity = _object_field(duplicate_started, "identity")
    duplicate_arguments = {
        "first_attempt": started_identity,
        "second_attempt": duplicate_identity,
        "verdict": "not_duplicate",
        "reason": "distinct tool-sweep attempts",
        "quantities": {
            "wall_micros": None,
            "token_count": None,
            "cost_micros": None,
            "test_count": None,
            "effect_count": None,
            "evidence": "owner_receipt",
            "effect_outcome": "not_applicable",
            "coverage": "known",
        },
        "format": "json",
    }
    call(
        "tracedecay_work_prepare_duplicate_adjudication",
        duplicate_arguments,
        deadline("tracedecay_work_prepare_duplicate_adjudication"),
    )
    call(
        "tracedecay_work_cancel_attempt",
        {
            "task_id": task_id,
            "run_id": run_id,
            "attempt_id": duplicate_attempt_id,
            "request_id": f"cancel.duplicate-probe.tool-sweep.{suffix}",
            "occurred_at": int(time.time() * 1_000_000),
            "format": "json",
        },
        deadline("tracedecay_work_cancel_attempt"),
    )

    fixture.update(
        {
            "work_selection": selection,
            "work_task_id": task_id,
            "work_run_id": run_id,
            "work_attempt_id": attempt_id,
            "work_initial_version": initial_version,
            "work_admitted_version": admitted_version,
            "work_execution_snapshot": execution_snapshot,
            "work_prepare_create_arguments": prepare_create,
            "work_generate_arguments": generate_arguments,
            "work_placement_arguments": placement_arguments,
            "work_status_arguments": status_arguments,
            "work_duplicate_arguments": duplicate_arguments,
            "work_duplicate_identity": duplicate_identity,
            "work_attempt_frontier": {
                "identity": started_identity,
                "state": cancelled_attempt["state"] if cancelled_attempt else "cancelled",
                "evidence_digest": None,
            },
            "work_effect_arguments": {
                "tracedecay_work_create": create_request,
                "tracedecay_work_accept_proposal": accept_request,
                "tracedecay_work_admit_execution": admit_request,
                "tracedecay_work_start_attempt": start_arguments,
                "tracedecay_work_cancel_attempt": cancel_arguments,
            },
        }
    )
    if effect_target in _WORK_LIFECYCLE_EFFECTS:
        fixture["work_effect_journey"] = _prepare_work_effect_journey(
            effect_target,
            fixture,
            call,
            deadline,
            started_identity,
            admitted_placement,
        )


def _work_status_identity(
    call: Call, deadline: Deadline, arguments: dict[str, Any], attempt_id: str,
) -> dict[str, Any]:
    status = call(
        "tracedecay_work_attempt_status",
        arguments,
        deadline("tracedecay_work_attempt_status"),
    )
    identity = _object_field(status, "identity")
    if identity.get("attempt_id") != attempt_id:
        raise JourneyError("Work status did not retain the produced attempt identity")
    return status


def _fresh_work_task(
    fixture: dict[str, Any],
    call: Call,
    deadline: Deadline,
    purpose: str,
    *,
    admit: bool,
) -> tuple[str, dict[str, Any], dict[str, Any] | None]:
    """Create a distinct task from the canonical prepared fixture shape."""
    suffix = f"{purpose}.{time.monotonic_ns()}"
    source = fixture["work_prepare_create_arguments"]
    original = source["change"]
    replacements = {
        original["initiative"]["id"]: f"initiative.{suffix}",
        original["plan"]["id"]: f"plan.{suffix}",
        original["milestone"]["id"]: f"milestone.{suffix}",
        original["item"]["input"]["task_id"]: f"task.{suffix}",
    }

    def replace(value: Any) -> Any:
        if isinstance(value, str):
            return replacements.get(value, value)
        if isinstance(value, list):
            return [replace(item) for item in value]
        if isinstance(value, dict):
            return {key: replace(item) for key, item in value.items()}
        return value

    create_prepare = replace(source)
    created_request = _object_field(
        call(
            "tracedecay_work_prepare_graph_mutation",
            create_prepare,
            deadline("tracedecay_work_prepare_graph_mutation"),
        ),
        "request",
    )
    call("tracedecay_work_create", created_request, deadline("tracedecay_work_create"))
    task_id = replacements[original["item"]["input"]["task_id"]]
    occurred_at = int(time.time() * 1_000_000)
    generated = call(
        "tracedecay_work_generate_proposal",
        {
            "selection": fixture["work_selection"],
            "task_id": task_id,
            "proposal_id": f"proposal.{suffix}",
            "occurred_at": occurred_at + 1,
            "format": "json",
        },
        deadline("tracedecay_work_generate_proposal"),
    )
    proposal = _object_field(generated, "proposal")
    if not admit:
        return task_id, proposal, None
    accepted_request = _object_field(
        call(
            "tracedecay_work_prepare_graph_mutation",
            {
                "selection": fixture["work_selection"],
                "change": {
                    "change": "decide_proposal",
                    "proposal": proposal,
                    "disposition": "accepted",
                },
                "evidence": [],
                "format": "json",
            },
            deadline("tracedecay_work_prepare_graph_mutation"),
        ),
        "request",
    )
    accepted = call(
        "tracedecay_work_accept_proposal",
        accepted_request,
        deadline("tracedecay_work_accept_proposal"),
    )
    accepted_version = _object_field(accepted, "verified_graph_version")
    admitted_request = _object_field(
        call(
            "tracedecay_work_prepare_graph_mutation",
            {
                "selection": fixture["work_selection"],
                "change": {
                    "change": "admit_execution",
                    "task_id": task_id,
                    "based_on_version": accepted_version["graph_version"],
                },
                "evidence": [],
                "format": "json",
            },
            deadline("tracedecay_work_prepare_graph_mutation"),
        ),
        "request",
    )
    admitted = call(
        "tracedecay_work_admit_execution",
        admitted_request,
        deadline("tracedecay_work_admit_execution"),
    )
    return task_id, proposal, _object_field(admitted, "execution_snapshot")


def _prepare_work_effect_journey(
    name: str,
    fixture: dict[str, Any],
    call: Call,
    deadline: Deadline,
    started_identity: dict[str, Any],
    admitted_placement: dict[str, Any],
) -> PreparedJourney:
    """Bind one Work effect to public producer output and prove its settlement."""
    occurred_at = int(time.time() * 1_000_000)
    if name in fixture.get("work_effect_arguments", {}):
        return _work_replay(name, fixture, call, deadline)

    if name == "tracedecay_work_mutate_graph":
        arguments = dict(fixture["work_effect_arguments"]["tracedecay_work_create"])

        def cleanup(response: dict[str, Any]) -> str:
            if not has_true(response, "replayed"):
                raise JourneyError("generic graph mutation did not replay its prepared mutation")
            call(
                "tracedecay_work_views",
                {"selection": fixture["work_selection"], "format": "json"},
                deadline("tracedecay_work_views"),
            )
            return "prepared graph mutation replay and graph view verified"

        return PreparedJourney(arguments, cleanup, "contained")

    if name == "tracedecay_work_resume_attempts":
        arguments = {"occurred_at": occurred_at, "format": "json"}

        def cleanup(response: dict[str, Any]) -> str:
            for field in ("recovery_required", "cancelled"):
                if not any(isinstance(value.get(field), list) for value in objects(response)):
                    raise JourneyError(f"attempt recovery omitted its typed {field} set")
            replay = call(name, arguments, deadline(name))
            if not any(
                isinstance(value.get("recovery_required"), list)
                for value in objects(replay)
            ):
                raise JourneyError("attempt recovery replay omitted its typed result")
            return "attempt recovery scan and idempotent rescan verified"

        return PreparedJourney(arguments, cleanup, "contained")

    if name in {"tracedecay_work_pause_run", "tracedecay_work_resume_run"}:
        pause_arguments = {
            "task_id": fixture["work_task_id"],
            "run_id": fixture["work_run_id"],
            "reason": "operator_request",
            "occurred_at": occurred_at,
            "format": "json",
        }
        if name == "tracedecay_work_pause_run":
            arguments = pause_arguments
        else:
            paused = call(
                "tracedecay_work_pause_run",
                pause_arguments,
                deadline("tracedecay_work_pause_run"),
            )
            authority = first_value(paused, {"authority"})
            if not isinstance(authority, int) or isinstance(authority, bool):
                raise JourneyError("Work pause omitted its authority version")
            arguments = {
                **pause_arguments,
                "expected_authority_version": authority,
                "occurred_at": occurred_at + 1,
            }

        def cleanup(response: dict[str, Any]) -> str:
            expected = "paused" if name.endswith("pause_run") else "running"
            if first_value(response, {"state"}) != expected:
                raise JourneyError(f"Work run control did not publish {expected}")
            if expected == "paused":
                authority = first_value(response, {"authority"})
                if not isinstance(authority, int) or isinstance(authority, bool):
                    raise JourneyError("Work pause omitted its authority version")
                call(
                    "tracedecay_work_resume_run",
                    {
                        **pause_arguments,
                        "expected_authority_version": authority,
                        "occurred_at": occurred_at + 1,
                    },
                    deadline("tracedecay_work_resume_run"),
                )
            reading = call(
                "tracedecay_work_run_control",
                {
                    "task_id": fixture["work_task_id"],
                    "run_id": fixture["work_run_id"],
                    "format": "json",
                },
                deadline("tracedecay_work_run_control"),
            )
            if first_value(reading, {"state"}) != "controlled":
                raise JourneyError("Work run-control read omitted the published aggregate")
            return "pause/resume transition and durable run-control read verified"

        return PreparedJourney(arguments, cleanup, "contained")

    if name in {"tracedecay_work_admit_placement", "tracedecay_work_release_placement"}:
        placement_arguments = dict(fixture["work_placement_arguments"])
        if name == "tracedecay_work_admit_placement":
            arguments = placement_arguments
        else:
            authority = first_value(admitted_placement, {"authority_version"})
            if not isinstance(authority, int) or isinstance(authority, bool):
                raise JourneyError("placement admission omitted its authority version")
            arguments = {
                "task_id": fixture["work_task_id"],
                "run_id": fixture["work_run_id"],
                "expected_authority_version": authority,
                "occurred_at": occurred_at,
                "format": "json",
            }

        def cleanup(response: dict[str, Any]) -> str:
            state = first_value(response, {"state"})
            if name.endswith("admit_placement"):
                authority = first_value(response, {"authority_version"})
                if not isinstance(authority, int) or isinstance(authority, bool):
                    raise JourneyError("placement replay omitted its authority version")
                released = call(
                    "tracedecay_work_release_placement",
                    {
                        "task_id": fixture["work_task_id"],
                        "run_id": fixture["work_run_id"],
                        "expected_authority_version": authority,
                        "occurred_at": occurred_at + 1,
                        "format": "json",
                    },
                    deadline("tracedecay_work_release_placement"),
                )
                state = first_value(released, {"state"})
            if state not in {"released", "quarantined"}:
                raise JourneyError("placement release omitted its truthful terminal state")
            status = call(
                "tracedecay_work_placement_status",
                {
                    "task_id": fixture["work_task_id"],
                    "run_id": fixture["work_run_id"],
                    "format": "json",
                },
                deadline("tracedecay_work_placement_status"),
            )
            if first_value(status, {"state"}) != "placed":
                raise JourneyError("placement status lost the terminal placement receipt")
            return "placement admission/release/status lifecycle verified"

        return PreparedJourney(arguments, cleanup, "contained")

    if name == "tracedecay_work_adjudicate_leak":
        arguments = {
            "adjudication_id": f"leak.tool-sweep.{time.monotonic_ns()}",
            "expected_revision": None,
            "attempt": started_identity,
            "detection_horizon_micros": 60_000_000,
            "command_id": f"command.leak.tool-sweep.{time.monotonic_ns()}",
            "format": "json",
        }

        def cleanup(response: dict[str, Any]) -> str:
            receipt = _object_field(response, "receipt")
            if _object_field(receipt, "command").get("command_id") != arguments["command_id"]:
                raise JourneyError("leak adjudication receipt changed its command identity")
            replay = call(name, arguments, deadline(name))
            replay_receipt = _object_field(replay, "receipt")
            if (
                _object_field(replay_receipt, "command").get("command_id")
                != arguments["command_id"]
            ):
                raise JourneyError("leak adjudication replay changed its command identity")
            return "mounted leak scan, receipt, and exact replay verified"

        return PreparedJourney(arguments, cleanup, "contained")

    if name == "tracedecay_work_review_proposal":
        _, review_proposal, _ = _fresh_work_task(
            fixture, call, deadline, "review.tool-sweep", admit=False
        )
        prepared = call(
            "tracedecay_work_prepare_graph_mutation",
            {
                "selection": fixture["work_selection"],
                "change": {
                    "change": "decide_proposal",
                    "proposal": review_proposal,
                    "disposition": "rejected",
                },
                "evidence": [],
                "format": "json",
            },
            deadline("tracedecay_work_prepare_graph_mutation"),
        )
        arguments = _object_field(prepared, "request")

        def cleanup(response: dict[str, Any]) -> str:
            replay = call(name, arguments, deadline(name))
            if not has_true(replay, "replayed"):
                raise JourneyError("proposal review did not exactly replay its retained decision")
            return "proposal generation/review/exact replay verified"

        return PreparedJourney(arguments, cleanup, "contained")

    if name == "tracedecay_work_synthesize":
        synthesis_task, _, synthesis_snapshot = _fresh_work_task(
            fixture, call, deadline, "synthesis.tool-sweep", admit=True
        )
        if synthesis_snapshot is None:
            raise JourneyError("synthesis task did not return an execution snapshot")
        arguments = {
            "start": {
                **fixture["work_effect_arguments"]["tracedecay_work_start_attempt"],
                "task_id": synthesis_task,
                "attempt_id": f"attempt.synthesis.tool-sweep.{time.monotonic_ns()}",
                "operation": "operation.work.synthesize",
                "execution_snapshot": synthesis_snapshot,
                "instructions": "Synthesize the exact source evidence.",
                "occurred_at": occurred_at,
            },
            "output_name": "inspection",
            "sources": [started_identity],
            "format": "json",
        }

        def cleanup(response: dict[str, Any]) -> str:
            synthesis = first_value(response, {"synthesis"})
            if synthesis not in {"admitted", "unsynthesized"}:
                raise JourneyError("synthesis omitted its typed admission outcome")
            replay = call(name, arguments, deadline(name))
            if first_value(replay, {"synthesis"}) != synthesis:
                raise JourneyError("synthesis replay changed its evidence outcome")
            if synthesis == "admitted":
                call(
                    "tracedecay_work_cancel_attempt",
                    {
                        "task_id": synthesis_task,
                        "run_id": arguments["start"]["run_id"],
                        "attempt_id": arguments["start"]["attempt_id"],
                        "request_id": f"cancel.synthesis.tool-sweep.{time.monotonic_ns()}",
                        "occurred_at": int(time.time() * 1_000_000),
                        "format": "json",
                    },
                    deadline("tracedecay_work_cancel_attempt"),
                )
            return "source attempt/synthesis task/typed outcome replay verified"

        return PreparedJourney(arguments, cleanup, "contained")

    if name == "tracedecay_work_retry_attempt":
        retry_attempt_id = f"attempt.retry-source.tool-sweep.{time.monotonic_ns()}"
        start_arguments = {
            **fixture["work_effect_arguments"]["tracedecay_work_start_attempt"],
            "attempt_id": retry_attempt_id,
            "occurred_at": occurred_at,
        }
        started = call(
            "tracedecay_work_start_attempt",
            start_arguments,
            deadline("tracedecay_work_start_attempt"),
        )
        identity = _object_field(started, "identity")
        status_arguments = {
            "task_id": fixture["work_task_id"],
            "run_id": fixture["work_run_id"],
            "attempt_id": retry_attempt_id,
            "format": "json",
        }
        terminal: dict[str, Any] | None = None
        terminal_state: str | None = None
        ends_at = time.monotonic() + 5
        while terminal is None and time.monotonic() < ends_at:
            status = _work_status_identity(
                call, deadline, status_arguments, retry_attempt_id
            )
            terminal_state = first_value(status, {"state"})
            terminal = next(
                (
                    value["terminal"]
                    for value in objects(status)
                    if isinstance(value.get("terminal"), dict)
                ),
                None,
            )
            if terminal is None:
                time.sleep(0.1)
        if terminal_state not in {"failed", "timed_out"} or terminal is None:
            raise JourneyError(
                "retry source did not publish runtime failure evidence in the bounded fixture"
            )
        evidence_digest = first_value(terminal, {"evidence_digest"})
        _manifest_digest(evidence_digest, "retry failure evidence digest")
        arguments = {
            "original_attempt": identity,
            "new_attempt_id": f"attempt.retry.tool-sweep.{time.monotonic_ns()}",
            "failure": {
                "source": "runtime",
                "cause": "runtime_failure",
                "evidence_ref": f"runtime-terminal:{evidence_digest}",
            },
            "command_id": f"command.retry.tool-sweep.{time.monotonic_ns()}",
            "format": "json",
        }

        def cleanup(response: dict[str, Any]) -> str:
            if first_value(response, {"outcome"}) != "created":
                raise JourneyError("first retry did not create a fresh attempt")
            receipt = _object_field(response, "receipt")
            if _object_field(receipt, "command").get("command_id") != arguments["command_id"]:
                raise JourneyError("retry receipt changed its command identity")
            replay = call(name, arguments, deadline(name))
            if first_value(replay, {"outcome"}) != "replayed":
                raise JourneyError("retry did not replay its durable receipt")
            call(
                "tracedecay_work_cancel_attempt",
                {
                    "task_id": fixture["work_task_id"],
                    "run_id": fixture["work_run_id"],
                    "attempt_id": arguments["new_attempt_id"],
                    "request_id": f"cancel.retry.tool-sweep.{time.monotonic_ns()}",
                    "occurred_at": int(time.time() * 1_000_000),
                    "format": "json",
                },
                deadline("tracedecay_work_cancel_attempt"),
            )
            return "runtime failure evidence/retry/new identity/replay verified"

        return PreparedJourney(arguments, cleanup, "contained")

    if name == "tracedecay_work_adjudicate_duplicate":
        second_identity = fixture["work_duplicate_identity"]
        prepared_duplicate = call(
            "tracedecay_work_prepare_duplicate_adjudication",
            fixture["work_duplicate_arguments"],
            deadline("tracedecay_work_prepare_duplicate_adjudication"),
        )
        arguments = next(
            (
                {**value, "format": "json"}
                for value in objects(prepared_duplicate)
                if value.get("first_attempt") == started_identity
                and value.get("second_attempt") == second_identity
                and isinstance(value.get("command_id"), str)
            ),
            None,
        )
        if arguments is None:
            raise JourneyError("duplicate producer omitted its prepared command")

        def cleanup(response: dict[str, Any]) -> str:
            receipt = _object_field(response, "receipt")
            command = _object_field(receipt, "command")
            if (
                command.get("first_attempt") != started_identity
                or command.get("second_attempt") != second_identity
            ):
                raise JourneyError("duplicate adjudication receipt changed its attempt identities")
            call(name, arguments, deadline(name))
            return "two attempts/owner evidence/adjudication replay verified"

        return PreparedJourney(arguments, cleanup, "contained")

    raise JourneyError(f"Work effect journey is not implemented for {name}")


def _git_hunk_input(response: dict[str, Any]) -> tuple[str, list[str]]:
    preview_input_id = first_value(response, {"preview_input_id"})
    digests = sorted(
        {
            value["digest"]
            for value in objects(response)
            if isinstance(value.get("digest"), str) and isinstance(value.get("hunk"), dict)
        }
    )
    if not isinstance(preview_input_id, str) or not preview_input_id or not digests:
        raise JourneyError("git hunks producer omitted its preview input or selected hunks")
    return preview_input_id, digests


def _git_preview(
    call: Call, deadline: Deadline, scope: str, operation: str,
) -> dict[str, Any]:
    hunks = call(
        "tracedecay_git_hunks",
        {"scope": scope, "format": "json"},
        deadline("tracedecay_git_hunks"),
    )
    preview_input_id, digests = _git_hunk_input(hunks)
    return call(
        "tracedecay_git_preview",
        {
            "operation": operation,
            "preview_input_id": preview_input_id,
            "selected_hunk_digests": digests,
            "format": "json",
        },
        deadline("tracedecay_git_preview"),
    )


def _git_apply(call: Call, deadline: Deadline) -> PreparedJourney:
    """Stage the real fixture hunk, verify it, then unstage through the same API."""
    preview = _git_preview(call, deadline, "working_tree", "stage_hunks")
    preview_id = first_value(preview, {"preview_id"})
    preview_digest = first_value(preview, {"preview_digest"})
    if not all(isinstance(value, str) and value for value in (preview_id, preview_digest)):
        raise JourneyError("git preview omitted the immutable apply capability")
    arguments = {
        "preview_id": preview_id,
        "preview_digest": preview_digest,
        "idempotency_key": f"tool-sweep-git-apply-{time.monotonic_ns()}",
        "format": "json",
    }

    def cleanup(response: dict[str, Any]) -> str:
        effect_id = first_value(response, {"effect_id"})
        if first_value(response, {"outcome"}) != "effect" or not isinstance(effect_id, str):
            raise JourneyError("git apply omitted its tagged durable effect receipt")
        staged = call(
            "tracedecay_git_hunks",
            {"scope": "staged", "format": "json"},
            deadline("tracedecay_git_hunks"),
        )
        _git_hunk_input(staged)
        replayed = call("tracedecay_git_apply", arguments, deadline("tracedecay_git_apply"))
        if first_value(replayed, {"effect_id"}) != effect_id:
            raise JourneyError("git apply retry changed its durable effect identity")

        inverse_preview = _git_preview(call, deadline, "staged", "unstage_hunks")
        inverse_id = first_value(inverse_preview, {"preview_id"})
        inverse_digest = first_value(inverse_preview, {"preview_digest"})
        if not all(isinstance(value, str) and value for value in (inverse_id, inverse_digest)):
            raise JourneyError("git inverse preview omitted its immutable capability")
        inverse = call(
            "tracedecay_git_apply",
            {
                "preview_id": inverse_id,
                "preview_digest": inverse_digest,
                "idempotency_key": f"tool-sweep-git-rollback-{time.monotonic_ns()}",
                "format": "json",
            },
            deadline("tracedecay_git_apply"),
        )
        if first_value(inverse, {"outcome"}) != "effect":
            raise JourneyError("git inverse apply omitted its tagged effect receipt")
        still_staged = call(
            "tracedecay_git_hunks",
            {"scope": "staged", "format": "json"},
            deadline("tracedecay_git_hunks"),
        )
        if any(isinstance(value.get("hunk"), dict) for value in objects(still_staged)):
            raise JourneyError("git inverse left the fixture hunk staged")
        restored = call(
            "tracedecay_git_hunks",
            {"scope": "working_tree", "format": "json"},
            deadline("tracedecay_git_hunks"),
        )
        _git_hunk_input(restored)
        return "hunks/preview/apply/staged/replay/inverse verified"

    return PreparedJourney(arguments, cleanup)


NATIVE_LIFECYCLE_EFFECTS = frozenset(
    {
        "tracedecay_multi_root_scope_set_compare_and_swap",
        "tracedecay_approve_native_integration",
        "tracedecay_apply_native_integration",
        "tracedecay_cancel_native_integration",
        "tracedecay_worktree_cleanup_remove",
    }
)


def _scope_set(response: dict[str, Any], scope_set_id: str) -> dict[str, Any]:
    for value in objects(response):
        if (
            value.get("scope_set_id") == scope_set_id
            and isinstance(value.get("revision"), int)
            and isinstance(value.get("digest"), str)
            and isinstance(value.get("roots"), list)
        ):
            return value
    raise JourneyError("scope-set producer omitted its persisted identity")


def _native_authority(response: dict[str, Any]) -> tuple[str, str]:
    for value in objects(response):
        policy = value.get("policy")
        if (
            isinstance(value.get("grant_digest"), str)
            and isinstance(policy, dict)
            and isinstance(policy.get("digest"), str)
        ):
            return value["grant_digest"], policy["digest"]
    raise JourneyError("native inventory omitted its grant and policy authority")


def _inventory_snapshot(response: dict[str, Any]) -> dict[str, Any]:
    for value in objects(response):
        if (
            value.get("state") == "snapshot"
            and isinstance(value.get("snapshot_id"), str)
            and isinstance(value.get("epoch"), int)
            and isinstance(value.get("entries"), list)
        ):
            return value
    raise JourneyError("worktree inventory omitted its exact snapshot")


def _root_scope(scope_set: dict[str, Any], root: str) -> dict[str, Any]:
    for value in scope_set["roots"]:
        if not isinstance(value, dict):
            continue
        locator = value.get("locator")
        scope = value.get("scope")
        if (
            isinstance(locator, dict)
            and locator.get("canonical_root") == root
            and isinstance(scope, dict)
        ):
            return scope
    raise JourneyError(f"scope set omitted registered root {root}")


def _inventory_entry(snapshot: dict[str, Any], worktree_id: str) -> dict[str, Any]:
    for value in snapshot["entries"]:
        if isinstance(value, dict) and value.get("worktree_id") == worktree_id:
            return value
    raise JourneyError(f"inventory omitted worktree {worktree_id}")


def _native_preview(response: dict[str, Any]) -> dict[str, Any]:
    for value in objects(response):
        if (
            isinstance(value.get("preview_id"), str)
            and isinstance(value.get("preview_digest"), str)
            and isinstance(value.get("ordered_commit_count"), int)
        ):
            return value
    raise JourneyError("native preflight omitted its immutable preview")


def _native_approval(response: dict[str, Any]) -> dict[str, Any]:
    for value in objects(response):
        if (
            isinstance(value.get("approval_id"), str)
            and isinstance(value.get("approval_digest"), str)
            and isinstance(value.get("transaction_id"), str)
        ):
            return value
    raise JourneyError("native approval omitted its daemon-minted transaction identity")


def _native_receipt(response: dict[str, Any], transaction_id: str) -> dict[str, Any]:
    for value in objects(response):
        status = value.get("status")
        if (
            isinstance(value.get("receipt_digest"), str)
            and isinstance(status, dict)
            and status.get("transaction_id") == transaction_id
        ):
            return value
    raise JourneyError("native apply omitted its durable transaction receipt")


def _worktree_inspection(response: dict[str, Any]) -> dict[str, Any]:
    for value in objects(response):
        if (
            value.get("state") == "inspection"
            and isinstance(value.get("inspection_digest"), str)
        ):
            return value
    raise JourneyError("worktree inspection omitted its exact digest")


def _worktree_confirmation(response: dict[str, Any]) -> dict[str, Any]:
    for value in objects(response):
        if (
            value.get("state") == "confirmed"
            and isinstance(value.get("confirmation_digest"), str)
            and isinstance(value.get("confirmed_at"), int)
        ):
            return value
    raise JourneyError("worktree confirmation omitted its exact cleanup proof")


def _recreate_cleanup_worktree(fixture: dict[str, Any]) -> None:
    completed = subprocess.run(
        [
            "git",
            "worktree",
            "add",
            "--quiet",
            fixture["cleanup_root"],
            fixture["cleanup_branch"],
        ],
        cwd=fixture["root"],
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        raise JourneyError(
            f"cleanup worktree recreation failed: {(completed.stderr or completed.stdout).strip()}"
        )


def _suspend_fixture_hunk(fixture: dict[str, Any]) -> None:
    path = Path(fixture["root"]) / "docs/large.md"
    suffix = "catalog sweep uncommitted hunk line\n"
    content = path.read_text()
    if not content.endswith(suffix):
        raise JourneyError("native integration fixture omitted its owned working-tree hunk")
    path.write_text(content[: -len(suffix)])
    fixture["native_suspended_hunk"] = content


def _restore_fixture_hunk(fixture: dict[str, Any]) -> None:
    content = fixture.pop("native_suspended_hunk", None)
    if not isinstance(content, str):
        return
    path = Path(fixture["root"]) / "docs/large.md"
    path.write_text(content)


def prime_native_admin_lifecycle(
    fixture: dict[str, Any],
    call: Call,
    deadline: Deadline,
    effect_target: str | None = None,
) -> None:
    """Use one persisted scope set for native integration and worktree cleanup."""
    scope_set_id = f"scope-set.tool-sweep.{time.monotonic_ns()}"
    roots = sorted(
        [
            {"project_id": fixture["project_id"], "root": fixture["root"]},
            {"project_id": fixture["project_id"], "root": fixture["cleanup_root"]},
        ],
        key=lambda value: (value["project_id"], value["root"]),
    )
    cas_arguments = {
        "scope_set_id": scope_set_id,
        "expected_revision": None,
        "roots": roots,
        "format": "json",
    }
    applied = call(
        "tracedecay_multi_root_scope_set_compare_and_swap",
        cas_arguments,
        deadline("tracedecay_multi_root_scope_set_compare_and_swap"),
    )
    scope_set = _scope_set(applied, scope_set_id)
    if len(scope_set["roots"]) != 2:
        raise JourneyError("scope-set CAS did not persist both exact worktrees")
    read_arguments = {"scope_set_id": scope_set_id, "format": "json"}
    read = call(
        "tracedecay_multi_root_scope_set_read",
        read_arguments,
        deadline("tracedecay_multi_root_scope_set_read"),
    )
    if _scope_set(read, scope_set_id)["digest"] != scope_set["digest"]:
        raise JourneyError("scope-set read changed the persisted digest")
    execute_arguments = {
        "scope_set_id": scope_set_id,
        "scope_set_revision": scope_set["revision"],
        "scope_set_digest": scope_set["digest"],
        "operation": {
            "kind": "git",
            "request": {"operation": "git_status", "request": {}},
        },
        "page": 0,
        "continuation": None,
        "format": "json",
    }
    executed = call(
        "tracedecay_multi_root_execute",
        execute_arguments,
        deadline("tracedecay_multi_root_execute"),
    )
    exact_scopes = {
        value.get("scope_digest")
        for value in objects(executed)
        if isinstance(value.get("scope_digest"), str)
        and isinstance(value.get("outcome"), dict)
        and value["outcome"].get("outcome") == "exact"
    }
    if len(exact_scopes) != 2:
        raise JourneyError("multi-root execute did not return exact Git status for both roots")

    # Native apply intentionally refuses a dirty destination. The sweep owns
    # exactly one working-tree hunk for the Git preview journey, so remove it
    # while the native lifecycle runs and restore the byte-exact fixture state
    # once the integration effect no longer needs a clean destination.
    _suspend_fixture_hunk(fixture)

    source_scope = _root_scope(scope_set, fixture["cleanup_root"])
    destination_scope = _root_scope(scope_set, fixture["root"])
    repository_target = {
        "kind": "repository",
        "project_id": source_scope["project_id"],
        "repository_id": source_scope["repository_id"],
    }
    binding = {
        "scope_set_id": scope_set_id,
        "scope_set_revision": scope_set["revision"],
        "scope_set_digest": scope_set["digest"],
    }
    inventory_arguments = {**binding, "target": repository_target, "format": "json"}
    inventory = call(
        "tracedecay_worktree_inventory",
        inventory_arguments,
        deadline("tracedecay_worktree_inventory"),
    )
    snapshot = _inventory_snapshot(inventory)
    grant_digest, policy_digest = _native_authority(inventory)
    source_entry = _inventory_entry(snapshot, source_scope["worktree_id"])
    destination_entry = _inventory_entry(snapshot, destination_scope["worktree_id"])
    if not all(
        isinstance(value, str) and value
        for value in (
            source_scope.get("reference"),
            destination_scope.get("reference"),
            source_entry.get("head"),
            destination_entry.get("head"),
        )
    ):
        raise JourneyError("inventory omitted exact source/destination refs or tips")

    source_node_id = "stack-node.tool-sweep.source"
    destination_node_id = "stack-node.tool-sweep.destination"
    nodes = [
        {
            "node_id": source_node_id,
            "project_id": source_scope["project_id"],
            "repository_id": source_scope["repository_id"],
            "worktree_id": source_scope["worktree_id"],
            "reference": source_scope["reference"],
            "tip": source_entry["head"],
        },
        {
            "node_id": destination_node_id,
            "project_id": destination_scope["project_id"],
            "repository_id": destination_scope["repository_id"],
            "worktree_id": destination_scope["worktree_id"],
            "reference": destination_scope["reference"],
            "tip": destination_entry["head"],
        },
    ]
    snapshot_arguments = {
        "source": source_scope,
        "destination": destination_scope,
        "authorized_scope_set_id": scope_set_id,
        "authorized_scope_set_revision": scope_set["revision"],
        "authorized_scope_set_digest": scope_set["digest"],
        "inventory_snapshot_id": snapshot["snapshot_id"],
        "inventory_epoch": snapshot["epoch"],
        "selection": {
            "kind": "declared_stack_edge",
            "binding": {
                "stack_id": "stack.tool-sweep",
                "revision_id": "stack-revision.tool-sweep.1",
                "nodes": nodes,
                "edges": [
                    {"dependency": source_node_id, "dependent": destination_node_id}
                ],
                "source_node_id": source_node_id,
                "destination_node_id": destination_node_id,
                "direction": "propagate_dependency_to_dependent",
            },
        },
        "grant_digest": grant_digest,
        "policy_digest": policy_digest,
        "format": "json",
    }
    stack = call(
        "tracedecay_stack_snapshot",
        snapshot_arguments,
        deadline("tracedecay_stack_snapshot"),
    )
    sealed_snapshot = _object_field(stack, "sealed_snapshot")
    preflight_arguments = {
        "snapshot": sealed_snapshot,
        "preferred_mode": "fast_forward",
        "format": "json",
    }
    preflight = call(
        "tracedecay_preflight_native_integration",
        preflight_arguments,
        deadline("tracedecay_preflight_native_integration"),
    )
    declared_preview = _native_preview(preflight)
    if declared_preview["ordered_commit_count"] < 1:
        raise JourneyError("native preflight found no source commit to integrate")

    # A declared edge into the active checkout is the authentic stack-signal
    # producer, but native apply correctly refuses to mutate an occupied ref.
    # Freeze the same returned repository/inventory authority as an independent
    # integration into an unoccupied fixture branch for the effect lifecycle.
    integration_snapshot_arguments = {
        **snapshot_arguments,
        "selection": {
            "kind": "independent_branch",
            "binding": {
                "proposal_digest": scope_set["digest"],
                "source_ref": source_scope["reference"],
                "destination_ref": f"refs/heads/{fixture['integration_branch']}",
            },
        },
    }
    integration_stack = call(
        "tracedecay_stack_snapshot",
        integration_snapshot_arguments,
        deadline("tracedecay_stack_snapshot"),
    )
    integration_preflight_arguments = {
        "snapshot": _object_field(integration_stack, "sealed_snapshot"),
        "preferred_mode": "fast_forward",
        "format": "json",
    }
    integration_preflight = call(
        "tracedecay_preflight_native_integration",
        integration_preflight_arguments,
        deadline("tracedecay_preflight_native_integration"),
    )
    preview = _native_preview(integration_preflight)
    if preview["ordered_commit_count"] < 1:
        raise JourneyError("native integration preflight found no source commit to integrate")
    approve_arguments = {
        "preview_id": preview["preview_id"],
        "preview_digest": preview["preview_digest"],
        "format": "json",
    }

    approval: dict[str, Any] | None = None
    if effect_target != "tracedecay_approve_native_integration":
        approval = _native_approval(
            call(
                "tracedecay_approve_native_integration",
                approve_arguments,
                deadline("tracedecay_approve_native_integration"),
            )
        )
    apply_arguments: dict[str, Any] | None = None
    receipt: dict[str, Any] | None = None
    if approval is not None:
        apply_arguments = {
            "preview_id": preview["preview_id"],
            "preview_digest": preview["preview_digest"],
            "approval_id": approval["approval_id"],
            "approval_digest": approval["approval_digest"],
            "transaction_id": approval["transaction_id"],
            "format": "json",
        }
        if effect_target != "tracedecay_apply_native_integration":
            receipt = _native_receipt(
                call(
                    "tracedecay_apply_native_integration",
                    apply_arguments,
                    deadline("tracedecay_apply_native_integration"),
                ),
                approval["transaction_id"],
            )
            status_arguments = {
                "transaction_id": approval["transaction_id"],
                "format": "json",
            }
            status = call(
                "tracedecay_native_integration_status",
                status_arguments,
                deadline("tracedecay_native_integration_status"),
            )
            if first_value(status, {"transaction_id"}) != approval["transaction_id"]:
                raise JourneyError("native status changed the applied transaction identity")
            cancel_arguments = dict(status_arguments)
            if effect_target != "tracedecay_cancel_native_integration":
                call(
                    "tracedecay_cancel_native_integration",
                    cancel_arguments,
                    deadline("tracedecay_cancel_native_integration"),
                )
            fixture.update(
                {
                    "native_status_arguments": status_arguments,
                    "native_cancel_arguments": cancel_arguments,
                }
            )

    if effect_target != "tracedecay_apply_native_integration":
        _restore_fixture_hunk(fixture)

    cleanup_target = {
        "kind": "worktree",
        "project_id": source_scope["project_id"],
        "repository_id": source_scope["repository_id"],
        "worktree_id": source_scope["worktree_id"],
    }
    inspect_arguments = {**binding, "target": cleanup_target, "format": "json"}
    inspection = _worktree_inspection(
        call(
            "tracedecay_worktree_cleanup_inspect",
            inspect_arguments,
            deadline("tracedecay_worktree_cleanup_inspect"),
        )
    )
    confirm_arguments = {
        **binding,
        "target": cleanup_target,
        "inspection_digest": inspection["inspection_digest"],
        "format": "json",
    }
    confirmation = _worktree_confirmation(
        call(
            "tracedecay_worktree_cleanup_confirm",
            confirm_arguments,
            deadline("tracedecay_worktree_cleanup_confirm"),
        )
    )
    remove_arguments = {
        **binding,
        "target": cleanup_target,
        "inspection_digest": inspection["inspection_digest"],
        "confirmed_at": confirmation["confirmed_at"],
        "confirmation_digest": confirmation["confirmation_digest"],
        "format": "json",
    }
    reconcile_arguments = {
        **binding,
        "target": cleanup_target,
        "confirmation_digest": confirmation["confirmation_digest"],
        "format": "json",
    }
    if effect_target != "tracedecay_worktree_cleanup_remove":
        removed = call(
            "tracedecay_worktree_cleanup_remove",
            remove_arguments,
            deadline("tracedecay_worktree_cleanup_remove"),
        )
        if first_value(removed, {"state"}) not in {"removed", "already_removed"}:
            raise JourneyError("worktree cleanup did not remove its confirmed target")
        reconciled = call(
            "tracedecay_worktree_cleanup_reconcile",
            reconcile_arguments,
            deadline("tracedecay_worktree_cleanup_reconcile"),
        )
        if first_value(reconciled, {"state"}) != "removed":
            raise JourneyError("worktree cleanup reconciliation did not prove removal")
        _recreate_cleanup_worktree(fixture)
        recreated = call(
            "tracedecay_worktree_inventory",
            inventory_arguments,
            deadline("tracedecay_worktree_inventory"),
        )
        recreated_snapshot = _inventory_snapshot(recreated)
        recreated_entry = _inventory_entry(
            recreated_snapshot, source_scope["worktree_id"]
        )
        if recreated_entry.get("presence") != "present":
            raise JourneyError("worktree inventory did not observe the recreated target")

    fixture.update(
        {
            "native_scope_set": scope_set,
            "native_scope_roots": roots,
            "native_read_arguments": {
                "tracedecay_multi_root_scope_set_read": read_arguments,
                "tracedecay_multi_root_execute": execute_arguments,
                "tracedecay_worktree_inventory": inventory_arguments,
                "tracedecay_stack_snapshot": snapshot_arguments,
                "tracedecay_preflight_native_integration": preflight_arguments,
                "tracedecay_worktree_cleanup_inspect": inspect_arguments,
                "tracedecay_worktree_cleanup_confirm": confirm_arguments,
                "tracedecay_worktree_cleanup_reconcile": reconcile_arguments,
                **(
                    {"tracedecay_native_integration_status": fixture["native_status_arguments"]}
                    if "native_status_arguments" in fixture
                    else {}
                ),
            },
            "native_effect_arguments": {
                "tracedecay_multi_root_scope_set_compare_and_swap": {
                    **cas_arguments,
                    "expected_revision": scope_set["revision"],
                },
                "tracedecay_approve_native_integration": approve_arguments,
                **(
                    {"tracedecay_apply_native_integration": apply_arguments}
                    if apply_arguments is not None
                    else {}
                ),
                **(
                    {"tracedecay_cancel_native_integration": fixture["native_cancel_arguments"]}
                    if "native_cancel_arguments" in fixture
                    else {}
                ),
                "tracedecay_worktree_cleanup_remove": remove_arguments,
            },
            "native_reconcile_arguments": reconcile_arguments,
            "native_inventory_arguments": inventory_arguments,
        }
    )


def _configuration_setting(
    call: Call, deadline: Deadline, key: str,
) -> tuple[str, dict[str, Any]]:
    response = call(
        "tracedecay_configuration_get",
        {"key": key, "format": "json"},
        deadline("tracedecay_configuration_get"),
    )
    setting = next(
        (
            value
            for value in objects(response)
            if value.get("key") == key and isinstance(value.get("effective_value"), dict)
        ),
        None,
    )
    if setting is None or not isinstance(setting.get("revision_id"), str):
        raise JourneyError(f"configuration get omitted {key}'s value or revision")
    return setting["revision_id"], setting["effective_value"]


def _configuration_receipt(response: dict[str, Any]) -> tuple[str, str]:
    receipt_id = first_value(response, {"receipt_id"})
    revision_id = first_value(response, {"result_revision_id"})
    if (
        first_value(response, {"outcome"}) != "effect"
        or not isinstance(receipt_id, str)
        or not isinstance(revision_id, str)
    ):
        raise JourneyError("configuration mutation omitted its tagged durable receipt")
    return receipt_id, revision_id


def _configuration_mutation(
    name: str, fixture: dict[str, Any], call: Call, deadline: Deadline,
) -> PreparedJourney:
    key = fixture["configuration_scalar_key"]
    baseline = fixture["configuration_scalar_value"]
    layer = {"kind": "project", "project_id": fixture["project_id"]}
    changed = {"kind": "boolean", "value": not baseline["value"]}

    def effect_arguments(
        tool: str, revision: str, value: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        common = {
            "expected_revision": revision,
            "idempotency_key": f"tool-sweep-{tool}-{time.monotonic_ns()}",
            "format": "json",
        }
        if tool == "tracedecay_configuration_batch":
            return {
                **common,
                "mutations": [{"operation": "set", "layer": layer, "key": key, "value": value}],
            }
        arguments = {**common, "layer": layer, "key": key}
        if value is not None:
            arguments["value"] = value
        return arguments

    baseline_revision, observed_baseline = _configuration_setting(call, deadline, key)
    if observed_baseline != baseline:
        raise JourneyError("configuration fixture baseline drifted before mutation")
    if name == "tracedecay_configuration_unset":
        seeded = effect_arguments("tracedecay_configuration_set", baseline_revision, changed)
        seeded_response = call(
            "tracedecay_configuration_set", seeded,
            deadline("tracedecay_configuration_set"),
        )
        _, current_revision = _configuration_receipt(seeded_response)
        arguments = effect_arguments(name, current_revision)
        expected_after = baseline
        rollback_value = changed
    else:
        arguments = effect_arguments(name, baseline_revision, changed)
        expected_after = changed
        rollback_value = None

    def cleanup(response: dict[str, Any]) -> str:
        receipt_id, result_revision = _configuration_receipt(response)
        replayed = call(name, arguments, deadline(name))
        if _configuration_receipt(replayed)[0] != receipt_id:
            raise JourneyError(f"{name} retry changed its durable receipt identity")
        observed_revision, observed = _configuration_setting(call, deadline, key)
        if observed_revision != result_revision or observed != expected_after:
            raise JourneyError(f"{name} consumer did not observe the committed value")
        rollback_tool = (
            "tracedecay_configuration_set"
            if rollback_value is not None
            else "tracedecay_configuration_unset"
        )
        rollback = effect_arguments(rollback_tool, result_revision, rollback_value)
        rolled_back = call(rollback_tool, rollback, deadline(rollback_tool))
        _, rollback_revision = _configuration_receipt(rolled_back)
        final_revision, final = _configuration_setting(call, deadline, key)
        expected_final = changed if rollback_value is not None else baseline
        if final_revision != rollback_revision or final != expected_final:
            raise JourneyError(f"{name} inverse did not restore its exact preimage")
        return "read/mutate/replay/consumer/inverse verified"

    return PreparedJourney(arguments, cleanup)


def _changed_topology_policy(policy: dict[str, Any]) -> dict[str, Any]:
    changed = json.loads(json.dumps(policy))
    allowed = changed.get("review_topology", {}).get("allowed")
    if not isinstance(allowed, list) or len(allowed) < 2:
        raise JourneyError("topology policy has no safely removable review mode")
    allowed.pop()
    return changed


def _configuration_plan_arguments(response: dict[str, Any]) -> dict[str, Any]:
    plan_id = first_value(response, {"plan_id"})
    base_revision = first_value(response, {"base_revision_id"})
    operation_digest = first_value(response, {"operation_digest"})
    if not all(
        isinstance(value, str) and value
        for value in (plan_id, base_revision, operation_digest)
    ):
        raise JourneyError("configuration preview omitted its immutable plan capability")
    return {
        "plan_id": plan_id,
        "expected_base_revision_id": base_revision,
        "operation_digest": operation_digest,
        "idempotency_key": f"tool-sweep-configuration-plan-{time.monotonic_ns()}",
        "format": "json",
    }


def _configuration_protected(
    name: str, fixture: dict[str, Any], call: Call, deadline: Deadline,
) -> PreparedJourney:
    key = fixture["configuration_key"]
    baseline_revision, baseline = _configuration_setting(call, deadline, key)
    changed = _changed_topology_policy(baseline["value"])
    preview = call(
        "tracedecay_configuration_protected_preview",
        {
            "change": {"kind": "replace_work_topology_policy", "value": changed},
            "expected_revision": baseline_revision,
            "format": "json",
        },
        deadline("tracedecay_configuration_protected_preview"),
    )
    apply_arguments = _configuration_plan_arguments(preview)
    if name == "tracedecay_configuration_rollback_apply":
        changed_response = call(
            "tracedecay_configuration_protected_apply",
            apply_arguments,
            deadline("tracedecay_configuration_protected_apply"),
        )
        _, changed_revision = _configuration_receipt(changed_response)
        rollback_preview = call(
            "tracedecay_configuration_rollback_preview",
            {
                "target_revision_id": baseline_revision,
                "mode": "all_or_nothing",
                "format": "json",
            },
            deadline("tracedecay_configuration_rollback_preview"),
        )
        arguments = _configuration_plan_arguments(rollback_preview)
        if arguments["expected_base_revision_id"] != changed_revision:
            raise JourneyError("rollback preview did not bind the committed protected revision")
        expected = baseline
    else:
        arguments = apply_arguments
        expected = {"kind": "work_topology_policy", "value": changed}

    def cleanup(response: dict[str, Any]) -> str:
        receipt_id, result_revision = _configuration_receipt(response)
        replayed = call(name, arguments, deadline(name))
        if _configuration_receipt(replayed)[0] != receipt_id:
            raise JourneyError(f"{name} retry changed its durable receipt identity")
        current_revision, current = _configuration_setting(call, deadline, key)
        if current_revision != result_revision or current != expected:
            raise JourneyError(f"{name} consumer did not observe the committed policy")
        if name == "tracedecay_configuration_protected_apply":
            rollback_preview = call(
                "tracedecay_configuration_rollback_preview",
                {
                    "target_revision_id": baseline_revision,
                    "mode": "all_or_nothing",
                    "format": "json",
                },
                deadline("tracedecay_configuration_rollback_preview"),
            )
            rollback = _configuration_plan_arguments(rollback_preview)
            rolled_back = call(
                "tracedecay_configuration_rollback_apply",
                rollback,
                deadline("tracedecay_configuration_rollback_apply"),
            )
            _, rollback_revision = _configuration_receipt(rolled_back)
            final_revision, final = _configuration_setting(call, deadline, key)
            if final_revision != rollback_revision or final != baseline:
                raise JourneyError("protected configuration rollback did not restore the baseline")
        return "preview/apply/replay/consumer/rollback verified"

    return PreparedJourney(arguments, cleanup)


def _source_apply(call: Call, tool: str, arguments: dict[str, Any], deadline: Deadline) -> dict[str, Any]:
    preview_arguments = {**arguments, "dry_run": True, "format": "json"}
    preview = call(tool, preview_arguments, deadline(tool))
    observed = expected_state(preview)
    if observed is None:
        # Large source previews deliberately return the normal retrieval handle
        # instead of an abbreviated, invented state. Consume that public handle.
        handle = response_handle(preview)
        if handle is not None:
            preview = call("tracedecay_retrieve", {"handle": handle}, deadline("tracedecay_retrieve"))
            observed = expected_state(preview)
    if observed is None:
        raise JourneyError(f"{tool} preview did not publish its expected_state")
    apply = {
        **arguments,
        "dry_run": False,
        "verify": tool == "tracedecay_rename_symbol",
        "idempotency_key": f"tool-sweep-{tool}-{time.monotonic_ns()}",
        "expected_state": observed,
        "format": "json",
    }
    if tool == "tracedecay_rename_symbol":
        apply["accepted_preview"] = _rename_acceptance(preview, observed)
    return apply


def _source_rollback(
    call: Call, tool: str, arguments: dict[str, Any], deadline: Deadline,
) -> None:
    rollback = _source_apply(call, tool, arguments, deadline)
    call(tool, rollback, deadline(tool))


def _source_snapshot(fixture: dict[str, str], paths: tuple[str, ...]) -> dict[str, str]:
    root = Path(fixture["root"])
    return {path: (root / path).read_text() for path in paths}


def _require_snapshot(fixture: dict[str, str], expected: dict[str, str], stage: str) -> None:
    observed = _source_snapshot(fixture, tuple(expected))
    if observed != expected:
        raise JourneyError(f"{stage} did not restore the exact source preimage")


def _effect_rollback_arguments(
    response: dict[str, Any], original_idempotency_key: str,
) -> dict[str, Any]:
    effect_id = first_value(response, {"effect_id"})
    input_digest = first_value(response, {"input_digest"})
    committed_state = first_value(response, {"committed_state"})
    if not all(
        isinstance(value, str) and value
        for value in (effect_id, input_digest, committed_state)
    ):
        raise JourneyError("source-edit receipt omitted the identities rollback consumes")
    return {
        "effect_id": effect_id,
        "original_idempotency_key": original_idempotency_key,
        "idempotency_key": f"tool-sweep-source-edit-rollback-{time.monotonic_ns()}",
        "original_input_digest": input_digest,
        "expected_state": committed_state,
        "confirm": True,
        "format": "json",
    }


def _rollback_effect(
    call: Call,
    deadline: Deadline,
    response: dict[str, Any],
    original_idempotency_key: str,
) -> None:
    effect_id = first_value(response, {"effect_id"})
    rolled_back = call(
        "tracedecay_source_edit_rollback",
        _effect_rollback_arguments(response, original_idempotency_key),
        deadline("tracedecay_source_edit_rollback"),
    )
    if not has_true(rolled_back, "success") or not has_true(rolled_back, "reconciled"):
        raise JourneyError("journaled rollback omitted its reconciled success receipt")
    rollback_effect = first_value(rolled_back, {"effect_id"})
    if not isinstance(rollback_effect, str) or not rollback_effect or rollback_effect == effect_id:
        raise JourneyError("journaled rollback did not mint its own durable effect identity")


def _rename_identity(
    call: Call, deadline: Deadline, node_id: str, new_name: str,
) -> dict[str, Any]:
    """Mint the exact rename identity from the read-only preview producer."""
    preview = call(
        "tracedecay_rename_preview",
        {"node_id": node_id, "new_name": new_name, "format": "json"},
        deadline("tracedecay_rename_preview"),
    )
    for value in objects(preview):
        node = value.get("node")
        if isinstance(node, dict) and all(
            isinstance(node.get(key), str) and node.get(key)
            for key in ("id", "qualified_name", "kind", "file", "name")
        ):
            identity: dict[str, Any] = {
                "node_id": node["id"],
                "qualified_name": node["qualified_name"],
                "kind": node["kind"],
                "file": node["file"],
                "old_name": node["name"],
            }
            return identity
    raise JourneyError("rename preview did not publish the exact symbol identity")


def _rename_acceptance(response: dict[str, Any], expected: str) -> dict[str, Any]:
    """Copy the immutable capability minted by rename_symbol's dry run."""
    for value in objects(response):
        preview_id = value.get("preview_id")
        preview_digest = value.get("preview_digest")
        plan_digest = value.get("plan_digest")
        graph_revision = value.get("graph_revision")
        repository_revision = value.get("repository_revision")
        if not all(
            isinstance(candidate, str) and _SHA256.fullmatch(candidate)
            for candidate in (preview_id, preview_digest, plan_digest, graph_revision)
        ):
            continue
        if repository_revision is not None and not isinstance(repository_revision, str):
            continue
        if preview_digest != expected:
            raise JourneyError("rename dry run published inconsistent preview and expected-state digests")
        return {
            "preview_id": preview_id,
            "preview_digest": preview_digest,
            "plan_digest": plan_digest,
            "repository_revision": repository_revision,
            "graph_revision": graph_revision,
        }
    raise JourneyError("rename dry run omitted its accepted_preview capability")


def _wait_for_code_symbol(
    call: Call, deadline: Deadline, name: str, qualified_name: str,
) -> dict[str, Any]:
    """Wait for the normal watcher to publish the edit's code generation."""
    ends_at = time.monotonic() + 30
    while True:
        result = call(
            "tracedecay_code_symbol_search",
            {
                "query": name,
                "lazy_index_ignored_dependencies": False,
                "scope": {},
                "meta": {"projection": "summary", "order": "relevance"},
                "format": "json",
            },
            deadline("tracedecay_code_symbol_search"),
        )
        match = next(
            (
                value
                for value in objects(result)
                if value.get("name") == name
                and value.get("qualified_name") == qualified_name
                and isinstance(value.get("node_id"), str)
            ),
            None,
        )
        if match is not None:
            return match
        if time.monotonic() >= ends_at:
            raise JourneyError(f"code index did not publish renamed symbol {qualified_name}")
        time.sleep(0.25)


def _source_edit(
    name: str, fixture: dict[str, str], call: Call, deadline: Deadline,
) -> PreparedJourney | None:
    file = fixture["file"]
    symbol = fixture["qualified_name"]
    original = _source_snapshot(fixture, ("src/lib.rs", "src/relocated.rs"))
    forward: dict[str, Any]
    inverse_tool = "tracedecay_str_replace"
    inverse: dict[str, Any]

    if name == "tracedecay_str_replace":
        forward = {"path": file, "old_str": "value: 7", "new_str": "value: 8"}
        inverse = {"path": file, "old_str": "value: 8", "new_str": "value: 7"}
    elif name == "tracedecay_multi_str_replace":
        forward = {
            "path": file,
            "replacements": [
                ["pub trait SweepTrait", "pub trait SweepTraitMutation"],
                ["pub struct SweepType", "pub struct SweepTypeMutation"],
            ],
        }
        inverse_tool = "tracedecay_multi_str_replace"
        inverse = {
            "path": file,
            "replacements": [
                ["pub trait SweepTraitMutation", "pub trait SweepTrait"],
                ["pub struct SweepTypeMutation", "pub struct SweepType"],
            ],
        }
    elif name == "tracedecay_insert_at":
        marker = "// tool sweep insert-at"
        forward = {
            "path": file,
            "anchor": "pub struct SweepType { pub value: i32 }",
            "content": marker,
            "before": False,
        }
        inverse = {"path": file, "old_str": f"\n{marker}\n", "new_str": "\n"}
    elif name == "tracedecay_ast_grep_rewrite":
        forward = {"path": file, "pattern": "SweepType { value: 7 }", "rewrite": "SweepType { value: 8 }"}
        inverse_tool = "tracedecay_ast_grep_rewrite"
        inverse = {"path": file, "pattern": "SweepType { value: 8 }", "rewrite": "SweepType { value: 7 }"}
    elif name == "tracedecay_replace_symbol":
        forward = {
            "symbol": symbol,
            "new_source": "pub fn sweep_anchor() -> SweepType { SweepType { value: 8 } }",
        }
        inverse_tool = "tracedecay_replace_symbol"
        inverse = {
            "symbol": symbol,
            "new_source": "pub fn sweep_anchor() -> SweepType { SweepType { value: 7 } }",
        }
    elif name == "tracedecay_insert_at_symbol":
        marker = "pub fn sweep_inserted() -> i32 { 11 }"
        forward = {"symbol": symbol, "content": marker, "position": "after"}
        inverse = {"path": file, "old_str": f"\n{marker}\n", "new_str": "\n"}
    elif name == "tracedecay_move_symbol":
        # This producer's ordinary Markdown preview intentionally summarizes
        # the move, while its published JSON contract carries expected_state.
        forward = {
            "symbol": symbol,
            "dest_file": "src/relocated.rs",
            "dry_run": False,
            "update_references": False,
            "format": "json",
        }
        inverse_tool = "tracedecay_move_symbol"
        inverse = {
            "symbol": symbol,
            "dest_file": file,
            "dry_run": False,
            "update_references": False,
            "format": "json",
        }
    elif name == "tracedecay_rename_symbol":
        renamed = f"{fixture['symbol']}_renamed"
        # The apply contract consumes the exact identity minted by the
        # read-only rename-preview producer, never a bare spelling.
        forward = {
            **_rename_identity(call, deadline, fixture["node_id"], renamed),
            "new_name": renamed,
        }
        inverse_tool = "tracedecay_rename_symbol"
        # The rename changes the symbol's identity; the rollback identity is
        # re-minted from the preview producer after the apply (see cleanup).
        inverse = {"new_name": fixture["symbol"]}
    elif name == "tracedecay_api_migration_apply":
        return _api_migration(fixture, call, deadline, original)
    else:
        return None

    apply = _source_apply(call, name, forward, deadline)

    def cleanup(response: dict[str, Any]) -> str:
        current = _source_snapshot(fixture, tuple(original))
        if current == original:
            raise JourneyError(f"{name} apply returned success without changing fixture source")
        if not has_true(response, "success"):
            raise JourneyError(f"{name} apply omitted its structured success receipt")
        effect_id = first_value(response, {"effect_id"})
        if not isinstance(effect_id, str) or not effect_id:
            raise JourneyError(f"{name} apply omitted its durable effect identity")
        if name == "tracedecay_rename_symbol":
            renamed_name = forward["new_name"]
            renamed_qualified = (
                f"{forward['qualified_name'].rsplit('::', 1)[0]}::{renamed_name}"
                if "::" in forward["qualified_name"]
                else renamed_name
            )
            renamed_node = _wait_for_code_symbol(
                call, deadline, renamed_name, renamed_qualified,
            )
            inverse.update(
                _rename_identity(
                    call, deadline, renamed_node["node_id"], forward["old_name"],
                )
            )
        replayed = call(name, apply, deadline(name))
        if not has_true(replayed, "replayed"):
            raise JourneyError(f"{name} idempotent retry did not replay its durable receipt")
        if first_value(replayed, {"effect_id"}) != effect_id:
            raise JourneyError(f"{name} idempotent retry changed its durable effect identity")
        if name == "tracedecay_move_symbol":
            _rollback_effect(call, deadline, response, apply["idempotency_key"])
            _require_snapshot(fixture, original, f"{name} rollback")
            return "preview/apply/consumer/journaled rollback verified"
        rollback_arguments = inverse
        _source_rollback(call, inverse_tool, rollback_arguments, deadline)
        _require_snapshot(fixture, original, f"{name} rollback")
        if name == "tracedecay_rename_symbol":
            _wait_for_code_symbol(
                call, deadline, forward["old_name"], forward["qualified_name"],
            )
            return "identity/dry-run/apply/reindex/replay/inverse verified"
        return "preview/apply/consumer/rollback verified"

    return PreparedJourney(apply, cleanup)


def _journaled_rollback(
    fixture: dict[str, str], call: Call, deadline: Deadline,
) -> PreparedJourney:
    """Produce one completed move_symbol effect whose receipt mints every
    rollback identity, then verify the journaled inverse restores the exact
    retained preimages."""
    original = _source_snapshot(fixture, ("src/lib.rs", "src/relocated.rs"))
    forward = {
        "symbol": fixture["qualified_name"],
        "dest_file": "src/relocated.rs",
        "dry_run": False,
        "update_references": False,
        "format": "json",
    }
    apply = _source_apply(call, "tracedecay_move_symbol", forward, deadline)
    applied = call("tracedecay_move_symbol", apply, deadline("tracedecay_move_symbol"))
    if not has_true(applied, "success"):
        raise JourneyError("rollback producer move did not complete successfully")
    if _source_snapshot(fixture, tuple(original)) == original:
        raise JourneyError("rollback producer move did not change fixture source")
    rollback_arguments = _effect_rollback_arguments(applied, apply["idempotency_key"])
    effect_id = rollback_arguments["effect_id"]

    def cleanup(response: dict[str, Any]) -> str:
        if not has_true(response, "success") or not has_true(response, "reconciled"):
            raise JourneyError("journaled rollback omitted its reconciled success receipt")
        rollback_effect = first_value(response, {"effect_id"})
        if not isinstance(rollback_effect, str) or not rollback_effect or rollback_effect == effect_id:
            raise JourneyError("journaled rollback did not mint its own durable effect identity")
        _require_snapshot(fixture, original, "source-edit journaled rollback")
        return "move producer/journaled inverse/preimage restoration verified"

    return PreparedJourney(
        rollback_arguments,
        cleanup,
    )


def _api_plan(response: dict[str, Any]) -> dict[str, Any] | None:
    for value in objects(response):
        digest = value.get("plan_digest")
        if isinstance(digest, str) and digest.startswith("sha256:") and isinstance(value.get("files"), list):
            return value
    return None


def api_migration_plan_arguments(fixture: dict[str, str]) -> dict[str, Any]:
    """Build one real, non-writing planner request from the node producer's identity."""
    marker = "pub fn sweep_compatibility() -> i32 { 11 }"
    identity = {
        "node_id": fixture["node_id"],
        "qualified_name": fixture["qualified_name"],
        "kind": fixture["node_kind"],
        "file": fixture["file"],
        "old_name": "sweep_anchor",
    }
    return {
        "family_id": "tool-sweep-compatibility",
        "operations": [{
            "kind": "insert_compatibility",
            "operation_id": "insert-sweep-compatibility",
            "anchor": identity,
            "position": "after",
            "definition": marker,
            "disposition": {
                "lifetime": "temporary",
                "external_consumer": "tool-sweep",
                "owner": "tool-sweep",
                "deprecation_policy": "remove after rollback verification",
                "deletion_condition": "catalog sweep rollback completed",
            },
        }],
    }


def _api_migration(
    fixture: dict[str, str], call: Call, deadline: Deadline, original: dict[str, str],
) -> PreparedJourney:
    marker = "pub fn sweep_compatibility() -> i32 { 11 }"
    plan_arguments = api_migration_plan_arguments(fixture)
    producer = call(
        "tracedecay_api_migration_plan",
        {
            **plan_arguments,
            # The planner's structured immutable plan is the producer consumed
            # by apply; ordinary tool consumers still use Markdown by default.
            "format": "json",
        },
        deadline("tracedecay_api_migration_plan"),
    )
    plan = _api_plan(producer)
    if plan is None:
        raise JourneyError("api-migration planner did not publish an immutable plan")
    digest = plan.get("plan_digest")
    if not isinstance(digest, str):
        raise JourneyError("api-migration planner omitted plan_digest")
    apply = _source_apply(
        call,
        "tracedecay_api_migration_apply",
        {"plan": plan, "plan_digest": digest, "dry_run": False, "verify": False},
        deadline,
    )

    def cleanup(response: dict[str, Any]) -> str:
        if marker not in (Path(fixture["root"]) / fixture["file"]).read_text():
            raise JourneyError("api-migration apply did not materialize the planned definition")
        if not has_true(response, "success"):
            raise JourneyError("api-migration apply omitted its structured success receipt")
        effect_id = first_value(response, {"effect_id"})
        if not isinstance(effect_id, str) or not effect_id:
            raise JourneyError("api-migration apply omitted its durable effect identity")
        replayed = call(
            "tracedecay_api_migration_apply",
            apply,
            deadline("tracedecay_api_migration_apply"),
        )
        if not has_true(replayed, "replayed"):
            raise JourneyError("api-migration retry did not replay its durable receipt")
        if first_value(replayed, {"effect_id"}) != effect_id:
            raise JourneyError("api-migration retry changed its durable effect identity")
        _source_rollback(
            call,
            "tracedecay_str_replace",
            {"path": fixture["file"], "old_str": f"\n{marker}\n", "new_str": "\n"},
            deadline,
        )
        _require_snapshot(fixture, original, "api-migration rollback")
        return "plan/apply/consumer/rollback verified"

    return PreparedJourney(apply, cleanup)


def profile_refresh_selectors(fixture: dict[str, str]) -> dict[str, Any]:
    """Select the mounted disposable profile without copying its internal identity."""
    return {
        "scope": {"kind": "profile"},
        "session": {"id": fixture["session_id"]},
        "source": {"scope": "codex"},
        "target": {
            "temporal_mode": {"kind": "current"},
            "grain": "session",
            "frontier": {"observed_through": 0, "committed_through": 0},
        },
        "format": "json",
    }


def _refresh_payload(response: dict[str, Any], family: str) -> dict[str, Any]:
    """Read the refresh payload through its canonical application envelope."""
    for value in objects(response):
        outcome = value.get("outcome")
        if not isinstance(outcome, dict) or outcome.get("outcome") != family:
            continue
        result = outcome.get("value")
        payload = result.get("payload") if isinstance(result, dict) else None
        if isinstance(payload, dict):
            return payload
    raise JourneyError(f"session refresh did not return a typed {family} payload")


def _terminal_refresh_state(response: dict[str, Any], operation_id: str) -> str:
    """The receipt-backed terminal state a durable cancel must return."""
    payload = _refresh_payload(response, "effect")
    terminal_state = payload.get("outcome")
    receipt = payload.get("receipt")
    if (
        not isinstance(receipt, dict)
        or receipt.get("operation_id") != operation_id
        or receipt.get("state") != terminal_state
        or terminal_state not in {"cancelled", "complete"}
    ):
        raise JourneyError(
            f"durable cancel did not return the operation's terminal receipt (state {terminal_state!r})"
        )
    return str(terminal_state)


def _require_settled_refresh(
    call: Call,
    deadline: Deadline,
    selectors: dict[str, Any],
    handle: str,
    operation_id: str,
    terminal_state: str,
) -> None:
    """Prove the terminal receipt stays durable through the read-only status route."""
    settled = call(
        "tracedecay_session_refresh_status",
        {"handle": handle, **selectors},
        deadline("tracedecay_session_refresh_status"),
    )
    payload = _refresh_payload(settled, "evidence")
    receipt = payload.get("receipt")
    if (
        payload.get("outcome") != terminal_state
        or not isinstance(receipt, dict)
        or receipt.get("operation_id") != operation_id
        or receipt.get("state") != terminal_state
    ):
        raise JourneyError("terminal refresh receipt did not stay durable after cancellation")


def _begun_refresh(response: dict[str, Any]) -> tuple[str, str]:
    """The opaque handle and durable operation identity a begin must return."""
    payload = _refresh_payload(response, "effect")
    outcome = payload.get("outcome")
    if outcome not in {"started", "joined"}:
        raise JourneyError(
            f"session refresh begin did not report started or joined (observed {outcome!r})"
        )
    handle = payload.get("handle")
    operation_id = payload.get("operation_id")
    if not handle or not isinstance(operation_id, str) or not operation_id:
        raise JourneyError("session refresh begin omitted its opaque handle or operation identity")
    return handle, operation_id


def _work_replay(
    name: str, fixture: dict[str, Any], call: Call, deadline: Deadline,
) -> PreparedJourney:
    arguments = fixture.get("work_effect_arguments", {}).get(name)
    if not isinstance(arguments, dict):
        raise JourneyError(f"shared Work lifecycle omitted replay arguments for {name}")

    def cleanup(response: dict[str, Any]) -> str:
        if name in {
            "tracedecay_work_create",
            "tracedecay_work_accept_proposal",
            "tracedecay_work_admit_execution",
        }:
            if not has_true(response, "replayed"):
                raise JourneyError(f"{name} did not replay its retained mutation")
        else:
            identity = _object_field(response, "identity")
            if identity.get("attempt_id") != fixture["work_attempt_id"]:
                raise JourneyError(f"{name} replay changed the attempt identity")
        status = call(
            "tracedecay_work_attempt_status",
            fixture["work_status_arguments"],
            deadline("tracedecay_work_attempt_status"),
        )
        if _object_field(status, "identity").get("attempt_id") != fixture["work_attempt_id"]:
            raise JourneyError(f"{name} replay lost the contained attempt")
        return "shared Work producer/effect/replay/status verified in disposable store"

    return PreparedJourney(dict(arguments), cleanup)


def _native_effect(
    name: str, fixture: dict[str, Any], call: Call, deadline: Deadline,
) -> PreparedJourney:
    arguments = fixture.get("native_effect_arguments", {}).get(name)
    if not isinstance(arguments, dict):
        raise JourneyError(f"shared native journey omitted arguments for {name}")

    def cleanup(response: dict[str, Any]) -> str:
        if name == "tracedecay_multi_root_scope_set_compare_and_swap":
            scope_set = _scope_set(response, fixture["native_scope_set"]["scope_set_id"])
            if scope_set["revision"] != fixture["native_scope_set"]["revision"] + 1:
                raise JourneyError("scope-set CAS did not advance the expected revision")
            read = call(
                "tracedecay_multi_root_scope_set_read",
                {"scope_set_id": scope_set["scope_set_id"], "format": "json"},
                deadline("tracedecay_multi_root_scope_set_read"),
            )
            if _scope_set(read, scope_set["scope_set_id"])["digest"] != scope_set["digest"]:
                raise JourneyError("scope-set CAS result was not durably readable")
            return "CAS/read verified in disposable scope-set store"
        if name == "tracedecay_approve_native_integration":
            approval = _native_approval(response)
            if approval["preview_id"] != arguments["preview_id"]:
                raise JourneyError("native approval changed the selected preview identity")
            return "preflight/approval/daemon-minted transaction identity verified"
        if name == "tracedecay_apply_native_integration":
            _native_receipt(response, arguments["transaction_id"])
            status_arguments = {
                "transaction_id": arguments["transaction_id"],
                "format": "json",
            }
            status = call(
                "tracedecay_native_integration_status",
                status_arguments,
                deadline("tracedecay_native_integration_status"),
            )
            if first_value(status, {"transaction_id"}) != arguments["transaction_id"]:
                raise JourneyError("native apply status lost its transaction identity")
            call(
                "tracedecay_cancel_native_integration",
                status_arguments,
                deadline("tracedecay_cancel_native_integration"),
            )
            _restore_fixture_hunk(fixture)
            return "inventory/snapshot/preflight/approve/apply/status/cancel verified"
        if name == "tracedecay_cancel_native_integration":
            status = call(
                "tracedecay_native_integration_status",
                fixture["native_status_arguments"],
                deadline("tracedecay_native_integration_status"),
            )
            if first_value(status, {"transaction_id"}) != arguments["transaction_id"]:
                raise JourneyError("native cancellation lost its transaction status")
            return "terminal apply/cancel/status verified"
        if name == "tracedecay_worktree_cleanup_remove":
            if first_value(response, {"state"}) not in {"removed", "already_removed"}:
                raise JourneyError("worktree cleanup effect did not remove its confirmed target")
            reconciled = call(
                "tracedecay_worktree_cleanup_reconcile",
                fixture["native_reconcile_arguments"],
                deadline("tracedecay_worktree_cleanup_reconcile"),
            )
            if first_value(reconciled, {"state"}) != "removed":
                raise JourneyError("worktree cleanup effect did not reconcile as removed")
            _recreate_cleanup_worktree(fixture)
            inventory = call(
                "tracedecay_worktree_inventory",
                fixture["native_inventory_arguments"],
                deadline("tracedecay_worktree_inventory"),
            )
            snapshot = _inventory_snapshot(inventory)
            target_id = arguments["target"]["worktree_id"]
            if _inventory_entry(snapshot, target_id).get("presence") != "present":
                raise JourneyError("recreated cleanup worktree was not present in inventory")
            return "inspect/confirm/remove/reconcile/recreate/inventory verified"
        raise JourneyError(f"no native effect journey for {name}")

    return PreparedJourney(dict(arguments), cleanup, "contained")


def _scout_claim(fixture: dict[str, Any], call: Call, deadline: Deadline, suffix: str) -> dict[str, Any]:
    response = call(
        "tracedecay_context_scout_claim",
        {"address": fixture["context_scout_address"], "window": "idle_window",
         "idempotency_key": f"tool-sweep-scout-claim-{suffix}"},
        deadline("tracedecay_context_scout_claim"),
    )
    claimed = next((value for value in objects(response)
                    if value.get("outcome") == "claimed" and isinstance(value.get("claim"), dict)), None)
    if claimed is None:
        raise JourneyError("Context Scout claim did not return its lease")
    return claimed["claim"]


def _prepare_context_scout(name: str, fixture: dict[str, Any], call: Call, deadline: Deadline) -> PreparedJourney:
    address = fixture["context_scout_address"]
    key = "context_scout.settings.v1"
    nonce = str(time.monotonic_ns())
    if name == "tracedecay_context_scout_pause":
        arguments = {"address": address, "expected_revision": fixture["context_scout_revision"],
                     "idempotency_key": f"tool-sweep-scout-pause-{nonce}"}
        def cleanup(_response: dict[str, Any]) -> str:
            revision, value = _configuration_setting(call, deadline, key)
            if value.get("value", {}).get("state") != "paused":
                raise JourneyError("Context Scout pause did not persist paused state")
            call("tracedecay_context_scout_resume",
                 {"address": address, "expected_revision": revision,
                  "idempotency_key": f"tool-sweep-scout-pause-rollback-{nonce}"},
                 deadline("tracedecay_context_scout_resume"))
            return "pause persisted and resume restored the disposable Scout"
        return PreparedJourney(arguments, cleanup)
    if name == "tracedecay_context_scout_resume":
        call("tracedecay_context_scout_pause",
             {"address": address, "expected_revision": fixture["context_scout_revision"],
              "idempotency_key": f"tool-sweep-scout-resume-setup-{nonce}"},
             deadline("tracedecay_context_scout_pause"))
        revision, _ = _configuration_setting(call, deadline, key)
        def cleanup(_response: dict[str, Any]) -> str:
            _, value = _configuration_setting(call, deadline, key)
            if value.get("value", {}).get("state") != "active":
                raise JourneyError("Context Scout resume did not restore active state")
            return "pause prerequisite and active resume state verified"
        return PreparedJourney({"address": address, "expected_revision": revision,
                                "idempotency_key": f"tool-sweep-scout-resume-{nonce}"}, cleanup)
    if name == "tracedecay_context_scout_claim":
        arguments = {"address": address, "window": "idle_window",
                     "idempotency_key": f"tool-sweep-scout-claim-{nonce}"}
        def cleanup(response: dict[str, Any]) -> str:
            claimed = next((value for value in objects(response)
                            if value.get("outcome") == "claimed" and isinstance(value.get("claim"), dict)), None)
            if claimed is None:
                raise JourneyError("Context Scout claim omitted its lease")
            call("tracedecay_context_scout_delivery",
                 {"address": address, "claim": claimed["claim"],
                  "delivered_at": int(time.time() * 1_000_000), "outcome": "displayed",
                  "idempotency_key": f"tool-sweep-scout-claim-settle-{nonce}"},
                 deadline("tracedecay_context_scout_delivery"))
            return "claim returned a real lease and delivery settled it"
        return PreparedJourney(arguments, cleanup, settlement="contained")
    if name in {"tracedecay_context_scout_delivery", "tracedecay_context_scout_feedback"}:
        claim = _scout_claim(fixture, call, deadline, nonce)
        delivery_arguments = {"address": address, "claim": claim,
                              "delivered_at": int(time.time() * 1_000_000), "outcome": "displayed",
                              "idempotency_key": f"tool-sweep-scout-delivery-{nonce}"}
        if name == "tracedecay_context_scout_delivery":
            def cleanup(response: dict[str, Any]) -> str:
                receipt = _object_field(response, "receipt")
                if not isinstance(receipt.get("receipt_id"), list):
                    raise JourneyError("Context Scout delivery omitted daemon receipt")
                return "real claim lease produced a daemon delivery receipt"
            return PreparedJourney(delivery_arguments, cleanup, settlement="contained")
        delivered = call("tracedecay_context_scout_delivery", delivery_arguments,
                         deadline("tracedecay_context_scout_delivery"))
        receipt = _object_field(delivered, "receipt")
        arguments = {"address": address, "receipt": receipt,
                     "feedback": {"receipt_id": receipt["receipt_id"], "kind": "explicitly_accepted"},
                     "idempotency_key": f"tool-sweep-scout-feedback-{nonce}"}
        def cleanup(_response: dict[str, Any]) -> str:
            recent = call("tracedecay_context_scout_recent", {"address": address, "limit": 8},
                          deadline("tracedecay_context_scout_recent"))
            if first_value(recent, {"kind"}) != "explicitly_accepted":
                raise JourneyError("Context Scout recent omitted recorded feedback")
            return "delivery receipt produced accepted feedback and recent retained it"
        return PreparedJourney(arguments, cleanup, settlement="contained")
    if name == "tracedecay_context_scout_cancel":
        work = fixture["context_scout_work"]
        arguments = {"address": address, "work": work,
                     "idempotency_key": f"tool-sweep-scout-cancel-{nonce}"}
        def cleanup(_response: dict[str, Any]) -> str:
            recent = call("tracedecay_context_scout_recent", {"address": address, "limit": 8},
                          deadline("tracedecay_context_scout_recent"))
            for value in objects(recent):
                if isinstance(value.get("pending"), list) and any(
                    isinstance(item, dict) and item.get("work") == work for item in value["pending"]
                ):
                    raise JourneyError("cancelled Context Scout work remained pending")
            return "pending work identity cancelled and disappeared from recent state"
        return PreparedJourney(arguments, cleanup, settlement="contained")
    raise JourneyError(f"unknown Context Scout mutation journey: {name}")



def prepare(
    name: str, client: Any, fixture: dict[str, str], deadline: Deadline, call: Call,
) -> PreparedJourney | None:
    """Prepare only cataloged journeys; unknown mutations stay visible failures."""
    if name.startswith("tracedecay_context_scout_"):
        return _prepare_context_scout(name, fixture, call, deadline)
    if name == "tracedecay_dashboard":
        def cleanup(response: dict[str, Any]) -> str:
            url = first_value(response, {"url", "dashboard_url"})
            if not isinstance(url, str) or not url.startswith("http://"):
                raise JourneyError("dashboard start omitted loopback URL")
            stopped = call(name, {"action": "stop", "format": "json"}, deadline(name))
            if not has_status(stopped, "stopped"):
                raise JourneyError("dashboard stop did not confirm listener termination")
            return "dashboard start/stop verified"
        return PreparedJourney(
            {"action": "start", "host": "127.0.0.1", "port": 0, "format": "json"},
            cleanup,
        )
    if name == "tracedecay_git_apply":
        return _git_apply(call, deadline)
    if name in NATIVE_LIFECYCLE_EFFECTS:
        return _native_effect(name, fixture, call, deadline)
    if name in NATIVE_LIFECYCLE_EFFECTS:
        return _native_effect(name, fixture, call, deadline)
    if name.startswith("tracedecay_workflow_"):
        prepared = fixture.get("workflow_effect_journey")
        if isinstance(prepared, PreparedJourney):
            return prepared
    if name.startswith("tracedecay_work_"):
        prepared = fixture.get("work_effect_journey")
        if isinstance(prepared, PreparedJourney):
            return prepared
    if name in {
        "tracedecay_work_create",
        "tracedecay_work_accept_proposal",
        "tracedecay_work_admit_execution",
        "tracedecay_work_start_attempt",
        "tracedecay_work_cancel_attempt",
    }:
        return _work_replay(name, fixture, call, deadline)
    if name in {
        "tracedecay_configuration_set",
        "tracedecay_configuration_unset",
        "tracedecay_configuration_batch",
    }:
        return _configuration_mutation(name, fixture, call, deadline)
    if name in {
        "tracedecay_configuration_protected_apply",
        "tracedecay_configuration_rollback_apply",
    }:
        return _configuration_protected(name, fixture, call, deadline)
    if name == "tracedecay_fact_store_add":
        content = "catalog sweep temporary isolated fact"
        def cleanup(response: dict[str, Any]) -> str:
            fact_id = fact_id_with_content(response, content)
            if fact_id is None:
                raise JourneyError("fact add omitted its structured fact identity")
            fetched = call(
                "tracedecay_fact_store_get",
                {"fact_id": fact_id, "format": "json"},
                deadline("tracedecay_fact_store_get"),
            )
            if fact_id_with_content(fetched, content) != fact_id:
                raise JourneyError("fact get did not consume the added fact identity")
            commit = _fact_commit(response, fact_id, {"committed", "idempotent_replay"})
            replayed = call(
                "tracedecay_fact_store_add",
                {
                    "content": content,
                    "category": "tool",
                    "trust": 0.5,
                    "source_label": "catalog_sweep",
                    "format": "json",
                },
                deadline("tracedecay_fact_store_add"),
            )
            replay = _fact_commit(replayed, fact_id, {"idempotent_replay"})
            if replay["last_event_id"] != commit["last_event_id"]:
                raise JourneyError("fact add retry appended a second retained event")
            _remove_seeded_fact(call, deadline, fact_id)
            listed = call(
                "tracedecay_fact_store_list",
                {"limit": 5, "format": "json"},
                deadline("tracedecay_fact_store_list"),
            )
            if fact_id_with_content(listed, content) == fact_id:
                raise JourneyError("fact containment did not verify default absence")
            return "fact add/get/replay/remove/tombstone/default-absence verified"
        return PreparedJourney(
            {
                "content": content,
                "category": "tool",
                "trust": 0.5,
                "source_label": "catalog_sweep",
                "format": "json",
            },
            cleanup,
            settlement="contained",
        )
    if name == "tracedecay_fact_store_update":
        original = "catalog sweep temporary fact before update"
        updated = "catalog sweep temporary fact after update"
        fact_id = _seeded_fact(call, deadline, original)

        def cleanup(response: dict[str, Any]) -> str:
            if fact_id_with_content(response, updated) != fact_id:
                raise JourneyError("fact update did not preserve the seeded fact identity")
            fetched = call(
                "tracedecay_fact_store_get",
                {"fact_id": fact_id, "format": "json"},
                deadline("tracedecay_fact_store_get"),
            )
            if fact_id_with_content(fetched, updated) != fact_id:
                raise JourneyError("fact get did not observe the updated content")
            commit = _fact_commit(response, fact_id, {"committed", "idempotent_replay"})
            replayed = call(
                "tracedecay_fact_store_update",
                {"fact_id": fact_id, "content": updated, "format": "json"},
                deadline("tracedecay_fact_store_update"),
            )
            replay = _fact_commit(replayed, fact_id, {"idempotent_replay"})
            if replay["last_event_id"] != commit["last_event_id"]:
                raise JourneyError("fact update retry appended a second retained event")
            _remove_seeded_fact(call, deadline, fact_id)
            return "fact update/get/replay verified; disposable fact contained by removal"

        return PreparedJourney(
            {"fact_id": fact_id, "content": updated, "format": "json"},
            cleanup,
            settlement="contained",
        )
    if name == "tracedecay_fact_store_remove":
        content = "catalog sweep temporary fact for removal"
        fact_id = _seeded_fact(call, deadline, content)

        def cleanup(response: dict[str, Any]) -> str:
            outcome = _fact_outcome(response, "removed", fact_id)
            commit = _fact_commit(response, fact_id, {"committed", "idempotent_replay"})
            fetched = call(
                "tracedecay_fact_store_get",
                {"fact_id": fact_id, "format": "json"},
                deadline("tracedecay_fact_store_get"),
            )
            if not _fact_unavailable(fetched, fact_id):
                raise JourneyError("fact remove did not preserve its deleted tombstone")
            listed = call(
                "tracedecay_fact_store_list",
                {"limit": 200, "format": "json"},
                deadline("tracedecay_fact_store_list"),
            )
            if fact_id_with_content(listed, content) == fact_id:
                raise JourneyError("fact remove did not verify absence")
            replayed = call(
                "tracedecay_fact_store_remove",
                {"fact_id": fact_id, "format": "json"},
                deadline("tracedecay_fact_store_remove"),
            )
            _fact_outcome(replayed, outcome["outcome"], fact_id)
            replay = _fact_commit(replayed, fact_id, {"idempotent_replay"})
            if replay["last_event_id"] != commit["last_event_id"]:
                raise JourneyError("fact remove retry appended a second retained event")
            return "irreversible fact remove/tombstone/default-absence/replay verified"

        return PreparedJourney(
            {"fact_id": fact_id, "format": "json"},
            cleanup,
            settlement="irreversible_verified",
        )
    if name == "tracedecay_fact_store_supersede":
        retired_content = "catalog sweep fact before supersession"
        successor_content = "catalog sweep fact after supersession"
        retired_id = _seeded_fact(call, deadline, retired_content)
        successor_id = _seeded_fact(call, deadline, successor_content)
        arguments = {
            "fact_id": retired_id,
            "superseded_by": successor_id,
            "format": "json",
        }

        def cleanup(response: dict[str, Any]) -> str:
            outcome = _fact_outcome(response, "superseded", retired_id)
            if outcome.get("superseded_by") != successor_id:
                raise JourneyError("fact supersede omitted its exact successor identity")
            commit = _fact_commit(response, retired_id, {"committed", "idempotent_replay"})
            last_event_id = commit["last_event_id"]

            listed = call(
                "tracedecay_fact_store_list",
                {"limit": 200, "format": "json"},
                deadline("tracedecay_fact_store_list"),
            )
            if fact_id_with_content(listed, retired_content) == retired_id:
                raise JourneyError("superseded fact remained on the default retrieval surface")
            if fact_id_with_content(listed, successor_content) != successor_id:
                raise JourneyError("supersession removed its current successor")

            retired = call(
                "tracedecay_fact_store_get",
                {"fact_id": retired_id, "format": "json"},
                deadline("tracedecay_fact_store_get"),
            )
            projection = next(
                (
                    value
                    for value in objects(retired)
                    if value.get("kind") == "superseded"
                    and value.get("superseded_by") == successor_id
                ),
                None,
            )
            if projection is None or not any(
                value.get("fact_id") == retired_id
                and value.get("content") == retired_content
                for value in objects(projection)
            ):
                raise JourneyError("exact fact retrieval omitted the retired projection")

            replayed = call(
                "tracedecay_fact_store_supersede",
                arguments,
                deadline("tracedecay_fact_store_supersede"),
            )
            replay = _fact_commit(replayed, retired_id, {"idempotent_replay"})
            if replay["last_event_id"] != last_event_id:
                raise JourneyError("fact supersede retry appended a second retirement event")
            return "fact add/supersede/list/exact-get/replay verified in disposable store"

        return PreparedJourney(arguments, cleanup, settlement="isolated")
    if name == "tracedecay_fact_feedback":
        content = "catalog sweep temporary feedback fact"
        fact_id = _seeded_fact(call, deadline, content)

        def cleanup(response: dict[str, Any]) -> str:
            feedback = next(
                (
                    value["feedback"]
                    for value in objects(response)
                    if isinstance(value.get("feedback"), dict)
                ),
                None,
            )
            if (
                first_value(response, {"outcome"}) != "effect"
                or not isinstance(feedback, dict)
                or feedback.get("action") != "helpful"
            ):
                raise JourneyError("fact feedback omitted its tagged helpful effect receipt")
            fetched = call(
                "tracedecay_fact_store_get",
                {"fact_id": fact_id, "format": "json"},
                deadline("tracedecay_fact_store_get"),
            )
            trust = _fact_trust(fetched, fact_id)
            if trust is None or trust <= 0.5:
                raise JourneyError(
                    f"helpful feedback did not raise trust above its 0.5 baseline (observed {trust})"
                )
            commit = _fact_commit(response, fact_id, {"committed", "idempotent_replay"})
            replayed = call(
                "tracedecay_fact_feedback",
                {
                    "fact_id": fact_id,
                    "action": "helpful",
                    "source_label": "catalog_sweep",
                    "format": "json",
                },
                deadline("tracedecay_fact_feedback"),
            )
            replay = _fact_commit(replayed, fact_id, {"idempotent_replay"})
            if replay["last_event_id"] != commit["last_event_id"]:
                raise JourneyError("fact feedback retry appended a second retained event")
            _remove_seeded_fact(call, deadline, fact_id)
            return "helpful feedback trust change/replay verified; disposable fact contained"

        return PreparedJourney(
            {"fact_id": fact_id, "action": "helpful", "source_label": "catalog_sweep", "format": "json"},
            cleanup,
            settlement="contained",
        )
    if name == "tracedecay_fact_store_curate":
        suffix = str(time.monotonic_ns())
        _seeded_fact(
            call,
            deadline,
            f"catalog sweep curator fact {suffix}",
            entities=[f"curator-alpha-{suffix}", f"curator-beta-{suffix}"],
            trust=0.8,
        )

        def cleanup(response: dict[str, Any]) -> str:
            settled_problem = next(
                (
                    value
                    for value in objects(response)
                    if value.get("kind") == "execution_failed"
                    and value.get("code")
                    == "application.automation-run.execution-failed"
                    and value.get("terminality") == "admitted_terminal"
                    and isinstance(value.get("request_id"), str)
                    and value["request_id"]
                ),
                None,
            )
            run_id = first_value(response, {"run_id"})
            if (not isinstance(run_id, str) or not run_id) and settled_problem is not None:
                # FactStoreCurateRequestV1 deliberately projects the transport
                # replay identity straight through as the durable run identity.
                run_id = settled_problem["request_id"]
            if not isinstance(run_id, str) or not run_id:
                raise JourneyError("fact curator omitted its durable run identity")
            if settled_problem is None and not any(
                value.get("run_id") == run_id and value.get("task") == "memory_curator"
                for value in objects(response)
            ):
                raise JourneyError("fact curator run is not bound to the Memory Curator")
            viewed = call(
                "tracedecay_automation_run_view",
                {"run_id": run_id, "format": "json"},
                deadline("tracedecay_automation_run_view"),
            )
            summary = next(
                (
                    value
                    for value in objects(viewed)
                    if value.get("run_id") == run_id
                    and value.get("task") == "memory_curator"
                    and value.get("status")
                    in ({"failed"} if settled_problem is not None else {"succeeded", "skipped"})
                ),
                None,
            )
            if summary is None:
                raise JourneyError("automation run view omitted the curator terminal")
            return f"Memory Curator run {run_id} retained terminal {summary['status']} evidence"

        return PreparedJourney(
            {
                "fact_review_limit": 24,
                "min_confidence_millionths": 720_000,
                "format": "json",
            },
            cleanup,
            settlement="isolated",
            accepted_terminal_problem=(
                "execution_failed",
                "application.automation-run.execution-failed",
            ),
        )
    if name == "tracedecay_memory_status":
        content = "catalog sweep temporary status fact"
        fact_id = _seeded_fact(call, deadline, content)

        def cleanup(response: dict[str, Any]) -> str:
            if not has_status(response, "ok"):
                raise JourneyError("memory status did not report its repaired ok status")
            counted = first_value(response, {"fact_count"})
            if not isinstance(counted, int) or counted < 1:
                raise JourneyError(
                    f"memory status did not count the seeded fact (observed {counted!r})"
                )
            _remove_seeded_fact(call, deadline, fact_id)
            return "memory status counted the seeded fact; producer fact removed"

        return PreparedJourney({"format": "json"}, cleanup)
    if name == "tracedecay_run_affected_tests":
        changed = fixture.get("file")
        if not changed:
            raise JourneyError("fixture did not record its seeded source file")

        def cleanup(response: dict[str, Any]) -> str:
            note = first_value(response, {"note"})
            if not isinstance(note, str) or "no tests cover" not in note:
                raise JourneyError(
                    "affected-test run did not report its truthful zero-coverage outcome"
                )
            # The journey call helper raises on any typed problem, so the
            # exact retained-result unavailability arrives as that raise.
            try:
                call(
                    "tracedecay_test_results",
                    {"format": "json"},
                    deadline("tracedecay_test_results"),
                )
            except Exception as error:
                if "application.retrieval.unavailable" not in str(error):
                    raise JourneyError(
                        f"zero-coverage retention check failed atypically: {error}"
                    ) from error
            else:
                raise JourneyError("zero-coverage run must retain no managed test result")
            return "zero-coverage run completed truthfully; no managed result retained"

        return PreparedJourney(
            {"changed_paths": [changed], "timeout_secs": 60, "max_tests": 5, "format": "json"},
            cleanup,
        )
    if name == "tracedecay_session_refresh_begin":
        selectors = profile_refresh_selectors(fixture)

        def cleanup(response: dict[str, Any]) -> str:
            handle, operation_id = _begun_refresh(response)
            cancelled = call(
                "tracedecay_session_refresh_cancel",
                {"handle": handle, **selectors},
                deadline("tracedecay_session_refresh_cancel"),
            )
            terminal_state = _terminal_refresh_state(cancelled, operation_id)
            _require_settled_refresh(call, deadline, selectors, handle, operation_id, terminal_state)
            return "durable refresh begin/cancel receipt verified terminal"

        return PreparedJourney(dict(selectors), cleanup)
    if name == "tracedecay_session_refresh_cancel":
        selectors = profile_refresh_selectors(fixture)
        handle, operation_id = _begun_refresh(
            call("tracedecay_session_refresh_begin", dict(selectors), deadline("tracedecay_session_refresh_begin"))
        )

        def cleanup(response: dict[str, Any]) -> str:
            terminal_state = _terminal_refresh_state(response, operation_id)
            _require_settled_refresh(call, deadline, selectors, handle, operation_id, terminal_state)
            return "durable refresh cancel receipt verified terminal"

        return PreparedJourney({"handle": handle, **selectors}, cleanup)
    if name == "tracedecay_source_edit_rollback":
        return _journaled_rollback(fixture, call, deadline)
    return _source_edit(name, fixture, call, deadline)
