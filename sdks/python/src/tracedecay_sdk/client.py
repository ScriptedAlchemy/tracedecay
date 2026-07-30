"""Typed synchronous client for TraceDecay HTTP and SSE lifecycle operations."""

from __future__ import annotations

import json
import time
import unicodedata
from collections.abc import Iterator
from dataclasses import dataclass
from typing import Any, Final, Literal, TypeAlias, TypeVar, TypedDict, cast
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlencode, urlsplit, urlunsplit
from urllib.request import Request, urlopen

from .operations import (
    OperationDescriptor,
    OperationResponse,
    PageOptionsLike,
    WorkOperations,
)
from .schema import JsonValue

JsonObject: TypeAlias = dict[str, JsonValue]
ConnectionKind: TypeAlias = Literal["local", "remote"]
RequestT = TypeVar("RequestT")
ResultT = TypeVar("ResultT")

_MAX_PAGE_SIZE: Final = 1_000
_MAX_OPAQUE_BYTES: Final = 4_096
_MAX_REQUEST_ID_BYTES: Final = 512
_MAX_RETRY_DELAY_MILLIS: Final = 30_000
_RETRY_DIRECTIVES: Final = frozenset(
    {"never", "same_request", "after_delay", "after_revalidate", "after_reconcile"}
)
_RETRY_SCOPE_BY_DIRECTIVE: Final = {
    "never": None,
    "same_request": "same_request",
    "after_delay": "same_request",
    "after_revalidate": "fresh_request",
    "after_reconcile": "same_operation",
}
_OWNING_LAYERS: Final = frozenset({"adapter", "application", "runtime", "port"})
_TERMINALITIES: Final = frozenset({"pre_admission", "admitted_terminal"})
_LEGAL_ACTIONS: Final = frozenset(
    {
        "correct_request",
        "reauthorize",
        "refresh",
        "retry",
        "reconcile",
        "contact_administrator",
    }
)
_PROBLEM_KINDS: Final = frozenset(
    {
        "invalid_request",
        "not_found_or_not_authorized",
        "conflict",
        "stale",
        "unsupported",
        "unavailable",
        "saturated",
        "cancelled",
        "timed_out",
    }
)
_TERMINAL_EVENTS: Final = frozenset(
    {"completed", "cancelled", "timed_out", "failed", "partial", "effect_unknown"}
)


class TraceDecayError(Exception):
    """Base error for SDK failures."""


class TraceDecayTransportError(TraceDecayError):
    """The daemon could not be reached or disconnected."""


class TraceDecayAuthenticationError(TraceDecayTransportError):
    """The daemon rejected bearer authentication or origin policy."""

    def __init__(self, status: int) -> None:
        super().__init__(f"daemon authentication failed with HTTP {status}")
        self.status = status


class TraceDecayProtocolError(TraceDecayError):
    """The request or daemon response violated the canonical protocol."""

    def __init__(self, message: str, *, status: int | None = None) -> None:
        super().__init__(message)
        self.status = status


class TraceDecayProblemError(TraceDecayError):
    """A canonical application problem returned by the daemon."""

    def __init__(self, status: int, envelope: JsonObject) -> None:
        problem, kind, code, message, retry = _validate_problem_envelope(envelope)
        super().__init__(f"{kind}/{code}: {message}")
        self.status = status
        self.envelope = envelope
        self.problem = problem
        self.kind = kind
        self.code = code
        self.retry = retry


@dataclass(frozen=True, slots=True)
class PageOptions:
    """Canonical request paging controls."""

    size: int | None = None
    cursor: str | None = None


@dataclass(frozen=True, slots=True)
class StreamResume:
    """Canonical resumable stream frontier."""

    token: str
    next_sequence: int


@dataclass(frozen=True, slots=True)
class StreamOptions:
    """Streaming and bounded reconnection policy."""

    resume: StreamResume | None = None
    max_reconnects: int = 0


@dataclass(frozen=True, slots=True)
class StreamEvent:
    """One validated SSE event."""

    event: str
    event_id: str | None
    data: JsonObject
    retry_delay_millis: int | None = None

    @property
    def terminal(self) -> bool:
        return self.event in _TERMINAL_EVENTS


class OperationCancellation(TypedDict):
    """Canonical cancellation acknowledgement."""

    status: Literal["requested", "already_requested", "already_terminal"]


class TraceDecayClient:
    """Synchronous local/remote client with identical lifecycle semantics."""

    def __init__(
        self,
        base_url: str,
        *,
        project_id: str,
        token: str,
        connection: ConnectionKind,
        origin: str | None = None,
        timeout: float = 30.0,
    ) -> None:
        parsed = urlsplit(base_url)
        if parsed.scheme not in {"http", "https"} or not parsed.netloc:
            raise TraceDecayProtocolError("base_url must be an absolute HTTP URL")
        if parsed.query or parsed.fragment:
            raise TraceDecayProtocolError("base_url must not contain query or fragment")
        if parsed.username is not None or parsed.password is not None:
            raise TraceDecayProtocolError("base_url must not contain credentials")
        _path_opaque(project_id, _MAX_REQUEST_ID_BYTES, "project_id")
        _query_opaque(token, _MAX_OPAQUE_BYTES, "token")
        normalized = urlunsplit(
            (parsed.scheme, parsed.netloc, parsed.path.rstrip("/"), "", "")
        ).rstrip("/")
        self._application_root = (
            f"{normalized}/projects/{quote(project_id, safe='.-_')}/application"
        )
        self._token = token
        self._origin = origin or f"{parsed.scheme}://{parsed.netloc}"
        self._timeout = timeout
        self.connection = connection
        self.operations = WorkOperations(self._request_operation)

    @classmethod
    def local(
        cls,
        base_url: str,
        *,
        project_id: str,
        token: str,
        timeout: float = 30.0,
    ) -> TraceDecayClient:
        return cls(
            base_url,
            project_id=project_id,
            token=token,
            connection="local",
            timeout=timeout,
        )

    @classmethod
    def remote(
        cls,
        base_url: str,
        *,
        project_id: str,
        token: str,
        origin: str,
        timeout: float = 30.0,
    ) -> TraceDecayClient:
        return cls(
            base_url,
            project_id=project_id,
            token=token,
            connection="remote",
            origin=origin,
            timeout=timeout,
        )

    def _request_operation(
        self,
        descriptor: OperationDescriptor[RequestT, ResultT],
        request: RequestT,
        *,
        page: PageOptionsLike | None = None,
    ) -> OperationResponse[ResultT]:
        """Invoke one operation admitted by canonical schema-body authority."""
        try:
            decoded_request = descriptor.decode_request(request)
        except TypeError as error:
            raise TraceDecayProtocolError(
                f"{descriptor.operation} request violates its canonical schema"
            ) from error
        query = _page_query(page)
        url = f"{self._project_root}{descriptor.route}"
        if query:
            url = f"{url}?{query}"
        body = json.dumps(decoded_request, separators=(",", ":")).encode()
        status, response = self._json_request(url, method="POST", body=body)
        kind = response.get("kind")
        value = _object(response.get("value"), "HTTP envelope value")
        if kind == "problem" and status >= 400:
            raise TraceDecayProblemError(status, value)
        if kind != "success" or status >= 400:
            raise TraceDecayProtocolError(
                "daemon returned an inconsistent application envelope", status=status
            )
        _string(value.get("request_id"), "success.request_id")
        if value.get("binding_id") != descriptor.binding_id:
            raise TraceDecayProtocolError(
                f"daemon returned a mismatched {descriptor.operation} binding",
                status=status,
            )
        contract = _object(value.get("contract"), "success.contract")
        if (
            contract.get("schema_id") != descriptor.result_schema_id
            or contract.get("schema_revision") != descriptor.result_schema_revision
        ):
            raise TraceDecayProtocolError(
                f"daemon returned a mismatched {descriptor.operation} result contract",
                status=status,
            )
        outcome = _object(value.get("outcome"), "success.outcome")
        outcome_value = _object(outcome.get("value"), "success.outcome.value")
        try:
            result = descriptor.decode_result(outcome_value.get("payload"))
        except TypeError as error:
            raise TraceDecayProtocolError(
                f"daemon returned a malformed {descriptor.operation} result",
                status=status,
            ) from error
        return OperationResponse(
            request_id=_string(value.get("request_id"), "success.request_id"),
            result=result,
        )

    def cancel_operation(self, operation_id: str) -> OperationCancellation:
        """Request cancellation of an accepted operation."""
        _path_opaque(operation_id, _MAX_REQUEST_ID_BYTES, "operation_id")
        url = self._lifecycle_url(operation_id, "cancel")
        status, value = self._json_request(url, method="POST")
        if value.get("kind") == "problem":
            if status < 400:
                raise TraceDecayProtocolError(
                    "daemon returned a problem with a successful HTTP status",
                    status=status,
                )
            raise TraceDecayProblemError(
                status, _object(value.get("value"), "cancellation problem")
            )
        cancellation = _string(value.get("status"), "cancellation.status")
        valid = (status, cancellation) in {
            (202, "requested"),
            (200, "already_requested"),
            (200, "already_terminal"),
        }
        if not valid:
            raise TraceDecayProtocolError(
                "daemon returned a non-canonical cancellation response", status=status
            )
        return cast(OperationCancellation, value)

    def stream_operation(
        self, operation_id: str, options: StreamOptions = StreamOptions()
    ) -> Iterator[StreamEvent]:
        """Stream operation events with bounded opt-in resume."""
        _path_opaque(operation_id, _MAX_REQUEST_ID_BYTES, "operation_id")
        if options.max_reconnects < 0:
            raise TraceDecayProtocolError("max_reconnects must be non-negative")
        next_sequence = options.resume.next_sequence if options.resume else None
        resume_token = options.resume.token if options.resume else None
        if resume_token is not None:
            _query_opaque(resume_token, _MAX_OPAQUE_BYTES, "resume token")
        reconnects = 0
        reconnect_delay_millis = 0
        while True:
            query: dict[str, str] = {}
            if next_sequence is not None:
                query["next_sequence"] = str(next_sequence)
            if resume_token is not None:
                query["resume_token"] = resume_token
            url = self._lifecycle_url(operation_id, "events")
            if query:
                url = f"{url}?{urlencode(query)}"
            try:
                with self._open(url, method="GET", accept="text/event-stream") as response:
                    media_type = response.headers.get_content_type()
                    if response.status >= 400 and media_type == "application/json":
                        try:
                            raw_problem = json.load(response)
                        except (UnicodeDecodeError, json.JSONDecodeError) as error:
                            raise TraceDecayProtocolError(
                                "daemon returned malformed stream problem JSON",
                                status=response.status,
                            ) from error
                        value = _object(raw_problem, "stream problem")
                        if value.get("kind") == "problem":
                            raise TraceDecayProblemError(
                                response.status,
                                _object(value.get("value"), "stream problem value"),
                            )
                    if media_type != "text/event-stream":
                        raise TraceDecayProtocolError(
                            "daemon did not open a canonical event stream",
                            status=response.status,
                        )
                    terminal = False
                    for event in _sse_events(response):
                        if event.retry_delay_millis is not None:
                            reconnect_delay_millis = min(
                                event.retry_delay_millis, _MAX_RETRY_DELAY_MILLIS
                            )
                        if event.event == "open":
                            if event.event_id is not None:
                                raise TraceDecayProtocolError(
                                    "SSE open event must not carry an ID"
                                )
                            open_data = _object(event.data.get("data"), "open.data")
                            if (
                                _string(
                                    open_data.get("correlation_id"),
                                    "open.correlation_id",
                                )
                                != operation_id
                            ):
                                raise TraceDecayProtocolError(
                                    "SSE open correlation identity does not match operation"
                                )
                            frontier = _object(
                                open_data.get("frontier"), "open.frontier"
                            )
                            next_sequence = _integer(
                                frontier.get("next_sequence"),
                                "frontier.next_sequence",
                            )
                            token_value = frontier.get("resume_token")
                            resume_token = (
                                None
                                if token_value is None
                                else _query_opaque(
                                    _string(token_value, "frontier.resume_token"),
                                    _MAX_OPAQUE_BYTES,
                                    "frontier.resume_token",
                                )
                            )
                            _integer(
                                frontier.get("retained_from_sequence"),
                                "frontier.retained_from_sequence",
                            )
                        else:
                            if event.event_id is None:
                                raise TraceDecayProtocolError(
                                    "SSE event is missing its canonical ID"
                                )
                            sequence = _integer(
                                _object(event.data.get("data"), "event.data").get(
                                    "sequence"
                                ),
                                "event.sequence",
                            )
                            if event.event_id != str(sequence):
                                raise TraceDecayProtocolError(
                                    "SSE ID disagrees with canonical sequence"
                                )
                            next_sequence = sequence + 1
                            if event.terminal:
                                _validate_terminal_event(event)
                        yield event
                        if event.terminal:
                            terminal = True
                            break
                    if terminal:
                        return
            except TraceDecayProtocolError:
                raise
            except (OSError, URLError) as error:
                if reconnects >= options.max_reconnects:
                    raise TraceDecayTransportError(str(error)) from error
            if reconnects >= options.max_reconnects:
                raise TraceDecayTransportError(
                    "event stream ended before a terminal event"
                )
            if next_sequence is None or resume_token is None:
                raise TraceDecayTransportError(
                    "event stream ended without a resumable frontier"
                )
            reconnects += 1
            time.sleep(reconnect_delay_millis / 1_000)

    @property
    def _project_root(self) -> str:
        return self._application_root.removesuffix("/application")

    def _lifecycle_url(self, operation_id: str, suffix: str) -> str:
        encoded = quote(operation_id, safe=".-_")
        return f"{self._application_root}/operations/{encoded}/{suffix}"

    def _headers(self, accept: str) -> dict[str, str]:
        return {
            "Accept": accept,
            "Authorization": f"Bearer {self._token}",
            "Origin": self._origin,
        }

    def _open(
        self,
        url: str,
        *,
        method: str,
        accept: str,
        body: bytes | None = None,
    ) -> Any:
        headers = self._headers(accept)
        if body is not None:
            headers["Content-Type"] = "application/json"
        request = Request(url, data=body, headers=headers, method=method)
        try:
            return urlopen(request, timeout=self._timeout)
        except HTTPError as error:
            if error.code in {401, 403}:
                error.close()
                raise TraceDecayAuthenticationError(error.code) from error
            return error
        except URLError as error:
            raise TraceDecayTransportError(str(error.reason)) from error

    def _json_request(
        self, url: str, *, method: str, body: bytes | None = None
    ) -> tuple[int, JsonObject]:
        with self._open(
            url, method=method, accept="application/json", body=body
        ) as response:
            if response.headers.get_content_type() != "application/json":
                raise TraceDecayProtocolError(
                    "daemon response is not application/json", status=response.status
                )
            try:
                value = json.load(response)
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise TraceDecayProtocolError(
                    "daemon returned malformed JSON", status=response.status
                ) from error
            return response.status, _object(value, "HTTP response")


def _sse_events(response: Any) -> Iterator[StreamEvent]:
    event_name = "message"
    event_id: str | None = None
    data: list[str] = []
    retry_delay_millis: int | None = None
    for raw_line in response:
        try:
            line = raw_line.decode("utf-8").rstrip("\r\n")
        except UnicodeDecodeError as error:
            raise TraceDecayProtocolError("SSE stream is not UTF-8") from error
        if line == "":
            if not data:
                event_name = "message"
                continue
            try:
                value = json.loads("\n".join(data))
            except json.JSONDecodeError as error:
                raise TraceDecayProtocolError("SSE data is not JSON") from error
            payload = _object(value, "SSE data")
            if payload.get("event") != event_name:
                raise TraceDecayProtocolError(
                    "SSE event name disagrees with JSON payload"
                )
            yield StreamEvent(event_name, event_id, payload, retry_delay_millis)
            event_name = "message"
            event_id = None
            data.clear()
            continue
        if line.startswith(":"):
            continue
        field, separator, raw_value = line.partition(":")
        field_value = raw_value.removeprefix(" ") if separator else ""
        if field == "event":
            event_name = field_value
        elif field == "id" and "\0" not in field_value:
            event_id = field_value
        elif field == "data":
            data.append(field_value)
        elif field == "retry" and field_value.isascii() and field_value.isdigit():
            retry_delay_millis = min(int(field_value), _MAX_RETRY_DELAY_MILLIS)
    if data:
        raise TraceDecayTransportError("event stream ended inside an SSE frame")


def _page_query(page: PageOptionsLike | None) -> str:
    if page is None:
        return ""
    query: dict[str, str] = {}
    if page.size is not None:
        if page.size < 1 or page.size > _MAX_PAGE_SIZE:
            raise TraceDecayProtocolError(
                f"page size must be between 1 and {_MAX_PAGE_SIZE}"
            )
        query["page_size"] = str(page.size)
    if page.cursor is not None:
        _query_opaque(page.cursor, _MAX_OPAQUE_BYTES, "page cursor")
        query["cursor"] = page.cursor
    return urlencode(query)


def _query_opaque(value: str, maximum: int, field: str) -> str:
    encoded = value.encode("utf-8")
    if (
        not value
        or value.strip() != value
        or len(encoded) > maximum
        or any(unicodedata.category(character) == "Cc" for character in value)
    ):
        raise TraceDecayProtocolError(f"{field} is not a canonical opaque value")
    return value


def _path_opaque(value: str, maximum: int, field: str) -> str:
    result = _query_opaque(value, maximum, field)
    if "/" in result:
        raise TraceDecayProtocolError(f"{field} is not a canonical path identifier")
    return result


def _validate_problem_envelope(
    envelope: JsonObject,
) -> tuple[JsonObject, str, str, str, str]:
    contract = _object(envelope.get("contract"), "problem contract")
    _string(contract.get("schema_id"), "problem contract.schema_id")
    revision = _integer(
        contract.get("schema_revision"), "problem contract.schema_revision"
    )
    if revision < 1:
        raise TraceDecayProtocolError("problem contract revision must be positive")
    request_id = _string(envelope.get("request_id"), "problem request_id")
    problem = _object(envelope.get("problem"), "problem")
    problem_revision = _integer(problem.get("revision"), "problem.revision")
    if problem_revision < 1:
        raise TraceDecayProtocolError("problem revision must be positive")
    kind = _string(problem.get("kind"), "problem.kind")
    if kind not in _PROBLEM_KINDS:
        raise TraceDecayProtocolError("problem.kind is not canonical")
    code = _string(problem.get("code"), "problem.code")
    message = _string(problem.get("message"), "problem.message")
    _nullable_diagnostic(problem.get("diagnostic"), "problem.diagnostic")
    owning_layer = _string(problem.get("owning_layer"), "problem.owning_layer")
    if owning_layer not in _OWNING_LAYERS:
        raise TraceDecayProtocolError("problem.owning_layer is not canonical")
    terminality = _string(problem.get("terminality"), "problem.terminality")
    if terminality not in _TERMINALITIES:
        raise TraceDecayProtocolError("problem.terminality is not canonical")
    retryable = problem.get("retryable")
    if not isinstance(retryable, bool):
        raise TraceDecayProtocolError("problem.retryable must be a boolean")
    retry = _string(problem.get("retry"), "problem.retry")
    if retry not in _RETRY_DIRECTIVES:
        raise TraceDecayProtocolError("problem.retry is not canonical")
    retry_scope = problem.get("retry_scope")
    if retry_scope is not None:
        _string(retry_scope, "problem.retry_scope")
    if retry_scope != _RETRY_SCOPE_BY_DIRECTIVE[retry]:
        raise TraceDecayProtocolError("problem.retry_scope is inconsistent with retry")
    retry_after = problem.get("retry_after_millis")
    if retry_after is not None:
        if retry != "after_delay":
            raise TraceDecayProtocolError(
                "retry_after_millis is only valid for after_delay"
            )
        _integer(retry_after, "problem.retry_after_millis")
    cancellation_stage = problem.get("cancellation_stage")
    if cancellation_stage is not None:
        _string(cancellation_stage, "problem.cancellation_stage")
    if _string(problem.get("request_id"), "problem.request_id") != request_id:
        raise TraceDecayProtocolError("problem request identity is inconsistent")
    _string(problem.get("trace_id"), "problem.trace_id")
    details = problem.get("details")
    if not isinstance(details, list):
        raise TraceDecayProtocolError("problem.details must be an array")
    for index, detail in enumerate(cast(list[object], details)):
        _nullable_diagnostic(detail, f"problem.details[{index}]")
    legal_actions = problem.get("legal_actions")
    if not isinstance(legal_actions, list) or not all(
        isinstance(action, str) for action in cast(list[object], legal_actions)
    ):
        raise TraceDecayProtocolError("problem.legal_actions must be a string array")
    for action in cast(list[str], legal_actions):
        if action not in _LEGAL_ACTIONS:
            raise TraceDecayProtocolError("problem.legal_actions is not canonical")
    if "coverage" not in problem:
        raise TraceDecayProtocolError("problem.coverage is required")
    if retryable != (retry != "never"):
        raise TraceDecayProtocolError("problem retryable and retry are inconsistent")
    return problem, kind, code, message, retry


def _nullable_diagnostic(value: object, field: str) -> None:
    if value is None:
        return
    diagnostic = _object(value, field)
    _string(diagnostic.get("code"), f"{field}.code")
    _string(diagnostic.get("message"), f"{field}.message")


def _validate_terminal_event(event: StreamEvent) -> None:
    event_data = _object(event.data.get("data"), "terminal.data")
    terminal = _object(event_data.get("terminal"), "terminal")
    termination = _string(terminal.get("termination"), "terminal.termination")
    receipt = _object(terminal.get("receipt"), "terminal.receipt")
    _integer(receipt.get("started_at"), "terminal.receipt.started_at")
    _integer(receipt.get("ended_at"), "terminal.receipt.ended_at")
    if "effective_deadline" not in receipt or "cancellation" not in receipt:
        raise TraceDecayProtocolError("terminal receipt lifecycle fields are required")
    budget = _object(receipt.get("budget"), "terminal.receipt.budget")
    _integer(budget.get("units_consumed"), "terminal.receipt.budget.units_consumed")
    _integer(budget.get("bytes_consumed"), "terminal.receipt.budget.bytes_consumed")
    _integer(budget.get("elapsed_micros"), "terminal.receipt.budget.elapsed_micros")
    receipt_termination = _string(
        receipt.get("termination"), "terminal.receipt.termination"
    )
    if termination != event.event or receipt_termination != event.event:
        raise TraceDecayProtocolError("terminal event termination is inconsistent")


def _object(value: object, field: str) -> JsonObject:
    if not isinstance(value, dict):
        raise TraceDecayProtocolError(f"{field} must be a JSON object")
    mapping = cast(dict[object, object], value)
    if not all(isinstance(key, str) for key in mapping):
        raise TraceDecayProtocolError(f"{field} must be a JSON object")
    return cast(JsonObject, mapping)


def _string(value: object, field: str) -> str:
    if not isinstance(value, str):
        raise TraceDecayProtocolError(f"{field} must be a string")
    return value


def _integer(value: object, field: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise TraceDecayProtocolError(f"{field} must be an unsigned integer")
    return value
