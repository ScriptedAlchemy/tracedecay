#!/usr/bin/env python3
"""Canonical V2 slice-authority normalization and fail-closed validation.

This module implements plan 00 §2.1 (``docs/plans/tracedecay-v2/00-plan-set-index.md``):
the deterministic ID normalizer, declaration/series/incidental classifier, owner/
companion reconciliation, typed-dependency and cycle validation, canonical digests,
stable idempotency keys, the bootstrap-manifest locator, and the pre/post-cutover
reconciliation comparison.

It is a read-only *validation projection*. It never dispatches work, computes
next-ready slices, mutates a graph, or manages leases/attempts/receipts — those are
execution concerns owned by ``plan_execution.py`` and the activated canonical graph.
The legacy ``plan_inventory.py`` heading/block-hash aid is intentionally NOT reused for
ID meaning; §2.1 forms are classified here independently (see SKILL.md).
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
import unicodedata
import urllib.parse
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

from git_observation import run_git

EN_DASH = "–"

# Canonical hashes are lowercase full object/block digests.
SHA256_HEX = re.compile(r"^[0-9a-f]{64}$")
COMMIT_OID = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")

# ---------------------------------------------------------------------------
# Diagnostics
# ---------------------------------------------------------------------------

ERROR_CODES = frozenset(
    {
        "missing_id",
        "malformed_id",
        "ambiguous_id",
        "missing_owner",
        "conflicting_owners",
        "conflicting_field",
        "invalid_series",
        "unresolved_dependency",
        "invalid_phase",
        "invalid_edge_type_or_payload",
        "source_anchor_mismatch",
        "digest_mismatch",
        "idempotency_mismatch",
        "duplicate_idempotency_key",
        "reconciliation_mismatch",
        "cycle",
    }
)
WARNING_CODES = frozenset(
    {"duplicate_description", "compatible_companion_addition", "incidental_reference"}
)

EDGE_KINDS = frozenset(
    {
        "requires_success",
        "requires_terminal",
        "requires_artifact",
        "requires_acceptance",
        "requires_decision",
        "requires_plan_outcome",
        "not_before",
    }
)
PAYLOAD_FREE_KINDS = frozenset({"requires_success"})
DEPENDENCY_PAYLOAD_FIELDS = {
    "requires_success": frozenset(),
    "requires_terminal": frozenset({"allowed"}),
    "requires_artifact": frozenset({"artifact_kind"}),
    "requires_acceptance": frozenset({"criterion"}),
    "requires_decision": frozenset({"decision", "allowed"}),
    "requires_plan_outcome": frozenset({"child_plan", "allowed"}),
    "not_before": frozenset({"not_before"}),
}

# Every declared edge kind gates dependency readiness except the purely temporal
# ``not_before``; only gating edges participate in whole-graph acyclicity (§2.1 step 5).
GATING_KINDS = EDGE_KINDS - {"not_before"}


@dataclass(frozen=True, order=True)
class Anchor:
    """Immutable source anchor: ``path:start_line-end_line`` plus block hash."""

    path: str
    start_line: int
    end_line: int
    block_sha256: str

    def as_dict(self) -> dict[str, object]:
        return {
            "path": self.path,
            "start_line": self.start_line,
            "end_line": self.end_line,
            "block_sha256": self.block_sha256,
        }

    def ref(self) -> str:
        return f"{self.path}:{self.start_line}-{self.end_line}#sha256:{self.block_sha256}"


@dataclass(frozen=True)
class Diagnostic:
    severity: str
    code: str
    normalized_id: str | None
    anchor: Anchor
    raw_value: str
    violated_rule: str
    suggestion: str | None = None

    @property
    def sort_key(self) -> tuple:
        return (
            self.code,
            self.normalized_id or "",
            self.anchor.path,
            self.anchor.start_line,
            self.anchor.end_line,
            self.anchor.block_sha256,
            self.raw_value,
            self.violated_rule,
        )


def _error(code: str, anchor: Anchor, raw: str, rule: str, nid: str | None = None,
           suggestion: str | None = None) -> Diagnostic:
    assert code in ERROR_CODES, code
    return Diagnostic("error", code, nid, anchor, raw, rule, suggestion)


def _warning(code: str, anchor: Anchor, raw: str, rule: str, nid: str | None = None) -> Diagnostic:
    assert code in WARNING_CODES, code
    return Diagnostic("warning", code, nid, anchor, raw, rule)


def sort_diagnostics(diagnostics: list[Diagnostic]) -> list[Diagnostic]:
    """Deduplicate byte-identical diagnostics and sort by §2.1's total key."""
    unique = {diag.sort_key + (diag.severity, diag.suggestion or ""): diag for diag in diagnostics}
    return sorted(unique.values(), key=lambda diag: diag.sort_key)


def validate_source_anchor(anchor: Anchor,
                           expected: "Anchor | str | None" = None) -> list[Diagnostic]:
    """Fail-closed structural validation of one source anchor (§2.1).

    Checks anchor bounds (non-empty path, ``1 <= start_line <= end_line``) and the block
    hash form (64-char lowercase hex SHA-256). When ``expected`` is supplied — either a
    full :class:`Anchor` or a bare hash string carried from a prior receipt/manifest — the
    actual block hash must equal the expected one. Every violation is a single
    ``source_anchor_mismatch`` error; an empty list means the anchor is well-formed and
    (if an expectation was given) verified. Validation only — never mutates the anchor.
    """
    diagnostics: list[Diagnostic] = []

    def mismatch(raw: str, rule: str) -> None:
        diagnostics.append(_error("source_anchor_mismatch", anchor, raw, rule))

    if not anchor.path:
        mismatch(anchor.path, "anchor path must be non-empty")
    if anchor.start_line < 1:
        mismatch(str(anchor.start_line), "anchor start line must be >= 1")
    if anchor.end_line < anchor.start_line:
        mismatch(str(anchor.end_line), "anchor end line must be >= start line")
    if not SHA256_HEX.match(anchor.block_sha256):
        mismatch(anchor.block_sha256, "block hash must be a 64-char lowercase sha-256")
    if expected is not None:
        expected_hash = expected.block_sha256 if isinstance(expected, Anchor) else expected
        if expected_hash != anchor.block_sha256:
            mismatch(anchor.block_sha256, "block hash does not match the expected anchor")
    return sort_diagnostics(diagnostics)


def block_sha256(lines: list[str]) -> str:
    """Hash logical inventory lines as UTF-8 joined by LF, without a terminal LF."""
    return hashlib.sha256("\n".join(lines).encode("utf-8")).hexdigest()


def _validate_pinned_anchor(anchor: Anchor, source: bytes | None) -> list[Diagnostic]:
    """Verify an anchor against one already-fetched pinned Git blob."""
    if source is None:
        return [_error("source_anchor_mismatch", anchor, anchor.path,
                       "anchor path is absent or unreadable in the pinned Git source commit")]
    try:
        lines = source.decode("utf-8").splitlines()
    except UnicodeDecodeError:
        return [_error("source_anchor_mismatch", anchor, anchor.path,
                       "pinned Git source block is not valid UTF-8")]
    if anchor.end_line > len(lines):
        return [_error("source_anchor_mismatch", anchor,
                       f"{anchor.start_line}-{anchor.end_line}",
                       "anchor line range is outside the pinned Git source block")]
    actual = block_sha256(lines[anchor.start_line - 1:anchor.end_line])
    if actual != anchor.block_sha256:
        return [_error("source_anchor_mismatch", anchor, anchor.block_sha256,
                       "block hash does not match the pinned Git source block")]
    return []


# ---------------------------------------------------------------------------
# Normalization (§2.1 rules 1-5)
# ---------------------------------------------------------------------------

NUM = r"(?:0|[1-9][0-9]*)"  # canonical unsigned integer, no leading zero
_SIMPLE = re.compile(rf"^{NUM}(?:[A-Z][A-Z0-9]*)?$")
_COMPOUND_BASE = re.compile(rf"^{NUM}[A-Z][A-Z0-9]*$")
_COMPONENT = re.compile(r"^(?:[A-Z]+(?:[1-9][0-9]*)?|[1-9][0-9]*)$")


@dataclass(frozen=True)
class Classification:
    """Result of classifying one raw declaration token."""

    kind: str  # "declaration" | "series" | "malformed"
    ids: tuple[str, ...] = ()  # canonical "PR <id>" values for a declaration
    series_id: str | None = None
    code: str | None = None  # malformed_id | ambiguous_id | invalid_series
    rule: str | None = None
    suggestion: str | None = None


def _ascii_upper(text: str) -> str:
    return "".join(chr(ord(ch) - 32) if "a" <= ch <= "z" else ch for ch in text)


def _compact(text: str) -> str:
    return re.sub(r"\s+", "", text)


def parse_scalar(token: str) -> tuple[str | None, str | None]:
    """Return ``(canonical, None)`` for a valid scalar, else ``(None, rule)``.

    ``token`` is already ASCII-uppercased and whitespace-free. Canonical form omits the
    leading ``PR``. Implements the scalar half of rules 1 (simple/compound), 3 (dotted).
    """
    if not token:
        return None, "empty scalar"
    if EN_DASH in token:
        return None, "en dash is a range delimiter, not part of a scalar"
    if "/" in token:
        return None, "slash is a list separator, not part of a scalar"
    hyphens = token.count("-")
    if hyphens > 1:
        return None, "a compound scalar carries exactly one identity-bearing hyphen"
    if hyphens == 1:
        base, _, component = token.partition("-")
        if not _COMPOUND_BASE.match(base):
            return None, "compound base requires a non-empty letter-led suffix"
        if not _COMPONENT.match(component):
            return None, (
                "compound component must be a letter run with an optional canonical "
                "decimal tail, or one canonical decimal"
            )
        return f"{base}-{component}", None
    if "." in token:
        num, _, tail = token.partition(".")
        if "." in tail:
            return None, "a dotted scalar carries at most one dot"
        if not re.match(rf"^{NUM}$", num):
            return None, "leading zeroes are forbidden"
        if re.match(r"^[A-Z]+$", tail):
            return f"{num}{tail}", None  # dotted letter suffix -> alternate spelling
        if re.match(r"^[1-9][0-9]*$", tail):
            return f"{num}.{tail}", None  # dotted numeric sub-ID is identity-bearing
        return None, "a dotted suffix is all letters or one canonical decimal"
    if not _SIMPLE.match(token):
        return None, "not a canonical simple scalar"
    return token, None


def _ascii_range(token: str) -> tuple[str, object]:
    """Test the three legacy ASCII simple-range productions in fixed order.

    Returns ``("ok", [members])`` when a production matches and expands, ``("reject",
    rule)`` when a production matches but the range is invalid (descending / oversized /
    leading zero), or ``("none", None)`` when no legacy production matches so the caller
    must fall through to the compound-scalar test (§2.1 rule 4 precedence).
    """
    numeric = re.fullmatch(rf"({NUM})-({NUM})", token)
    if numeric:
        return _expand_numeric(numeric.group(1), numeric.group(2))
    letters = re.fullmatch(rf"({NUM})([A-Z])-({NUM})([A-Z])", token)
    if letters:
        if letters.group(1) != letters.group(3):
            return "reject", "letter range must share one numeric stem"
        return _expand_letters(letters.group(1), letters.group(2), letters.group(4))
    tail = re.fullmatch(rf"({NUM}[A-Z])([0-9]+)-({NUM}[A-Z])([0-9]+)", token)
    if tail:
        if tail.group(1) != tail.group(3):
            return "reject", "numeric-tail range must share one letter stem"
        return _expand_tail(tail.group(1), tail.group(2), tail.group(4))
    return "none", None


def _bounded(members: list[str]) -> tuple[str, object]:
    if len(members) > 1000:
        return "reject", "a range may not exceed 1,000 members"
    return "ok", members


def _expand_numeric(start: str, end: str) -> tuple[str, object]:
    first, last = int(start), int(end)
    if first > last:
        return "reject", "a range must ascend"
    return _bounded([str(value) for value in range(first, last + 1)])


def _expand_letters(stem: str, start: str, end: str) -> tuple[str, object]:
    first, last = ord(start), ord(end)
    if first > last:
        return "reject", "a range must ascend"
    return _bounded([f"{stem}{chr(value)}" for value in range(first, last + 1)])


def _expand_tail(stem: str, start: str, end: str) -> tuple[str, object]:
    if (len(start) > 1 and start[0] == "0") or (len(end) > 1 and end[0] == "0"):
        return "reject", "range tails must be canonical decimals"
    first, last = int(start), int(end)
    if first > last:
        return "reject", "a range must ascend"
    return _bounded([f"{stem}{value}" for value in range(first, last + 1)])


def _endash_range(token: str) -> tuple[str | None, str | None]:
    """Expand a U+2013 range: two complete same-shape/stem scalar endpoints."""
    parts = token.split(EN_DASH)
    if len(parts) != 2 or "" in parts:
        return None, "an en-dash range needs exactly two endpoints"
    left, right = parts
    numeric = re.fullmatch(rf"({NUM})", left) and re.fullmatch(rf"({NUM})", right)
    if numeric:
        status, result = _expand_numeric(left, right)
        return (None, result) if status == "reject" else (result, None)
    for pattern, expand in (
        (rf"({NUM})([A-Z])", lambda l, r: _expand_letters(l.group(1), l.group(2), r.group(2))
         if l.group(1) == r.group(1) else ("reject", "endpoints must share one stem")),
        (rf"({NUM}[A-Z])([0-9]+)", lambda l, r: _expand_tail(l.group(1), l.group(2), r.group(2))
         if l.group(1) == r.group(1) else ("reject", "endpoints must share one stem")),
        (rf"({NUM}[A-Z][A-Z0-9]*)-([A-Z]+)([1-9][0-9]*)",
         lambda l, r: _expand_tail(f"{l.group(1)}-{l.group(2)}", l.group(3), r.group(3))
         if (l.group(1), l.group(2)) == (r.group(1), r.group(2))
         else ("reject", "compound endpoints must share base and component letters")),
    ):
        lmatch = re.fullmatch(pattern, left)
        rmatch = re.fullmatch(pattern, right)
        if lmatch and rmatch:
            status, result = expand(lmatch, rmatch)
            return (None, result) if status == "reject" else (result, None)
    return None, "en-dash endpoints are not a valid, complete, same-shape range"


def _classify_slash(compact: str) -> Classification:
    parts = compact.split("/")
    if "" in parts:
        return Classification("malformed", code="malformed_id",
                              rule="a slash list has no empty member")
    if (
        len(parts) == 2
        and re.fullmatch(rf"{NUM}", parts[0])
        and re.fullmatch(r"[A-Z][A-Z0-9]*", parts[1])
    ):
        return Classification("declaration", ids=(f"PR {parts[0]}{parts[1]}",))
    ids: list[str] = []
    for part in parts:
        canonical, rule = parse_scalar(part)
        if canonical is None:
            return Classification("malformed", code="malformed_id",
                                  rule=f"slash member is not a complete scalar: {rule}")
        ids.append(f"PR {canonical}")
    return Classification("declaration", ids=tuple(dict.fromkeys(ids)))


def classify_token(raw: str) -> Classification:
    """Classify one raw declaration token per §2.1 rules 1-5 (fail-closed)."""
    text = re.sub(r"(?i)^\s*PR\b", "", raw.strip(), count=1).strip()
    if not text:
        return Classification("malformed", code="malformed_id",
                              rule="declaration carries no identifier")
    upper = _ascii_upper(text)

    series = re.fullmatch(r"(.+?)\s+SERIES", upper)
    if series:
        canonical, rule = parse_scalar(_compact(series.group(1)))
        if canonical is None:
            return Classification("malformed", code="invalid_series",
                                  rule=f"series stem is not a scalar: {rule}")
        return Classification("series", series_id=f"PR {canonical} series")

    compact = _compact(upper)
    if "/" in compact:
        return _classify_slash(compact)
    if EN_DASH in compact:
        members, rule = _endash_range(compact)
        if members is None:
            return Classification("malformed", code="malformed_id", rule=rule)
        return Classification("declaration", ids=tuple(f"PR {member}" for member in members))
    if "-" in compact:
        status, result = _ascii_range(compact)
        if status == "ok":
            return Classification("declaration", ids=tuple(f"PR {member}" for member in result))
        if status == "reject":
            return Classification("malformed", code="malformed_id", rule=str(result))
        # status == "none": fall through and test the whole token as a compound scalar.
    canonical, rule = parse_scalar(compact)
    if canonical is None:
        return Classification("malformed", code="malformed_id", rule=rule)
    return Classification("declaration", ids=(f"PR {canonical}",))


# ---------------------------------------------------------------------------
# Reconciliation (§2.1 steps 4-5)
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Criterion:
    criterion_id: str
    text: str
    source_anchors: tuple[str, ...] = ()


@dataclass(frozen=True)
class Dependency:
    parent: str
    kind: str
    payload: tuple[tuple[str, object], ...] = ()
    source_anchor: str | None = None
    source_anchors: tuple[str, ...] = ()

    def all_source_anchors(self) -> tuple[str, ...]:
        values = set(self.source_anchors)
        if self.source_anchor is not None:
            values.add(self.source_anchor)
        return tuple(sorted(values))


@dataclass(frozen=True)
class Section:
    """One declaring (or incidental) section for a slice."""

    raw_id: str
    role: str  # "owner" | "companion"
    anchor: Anchor
    heading: str = ""
    phase: int | None = None
    commit_subject: str | None = None
    acceptance: tuple[Criterion, ...] = ()
    dependencies: tuple[Dependency, ...] = ()
    incidental: bool = False


@dataclass
class SliceRecord:
    normalized_id: str
    owner: Anchor
    owner_heading: str = ""
    companions: list[Anchor] = field(default_factory=list)
    phase: int | None = None
    commit_subject: str | None = None
    acceptance: list[Criterion] = field(default_factory=list)
    dependencies: list[Dependency] = field(default_factory=list)
    source_anchors: list[Anchor] = field(default_factory=list)
    content_digest: str = ""
    idempotency_key: str = ""

    def reconciled_body(self) -> dict[str, object]:
        """The digested body: everything except digest/key/lifecycle fields (§2.1)."""
        return {
            "normalized_id": self.normalized_id,
            "owner": {
                "path": self.owner.path,
                "heading": self.owner_heading,
                "anchor": {
                    "start_line": self.owner.start_line,
                    "end_line": self.owner.end_line,
                    "block_sha256": self.owner.block_sha256,
                },
            },
            "companions": [
                {"path": anchor.path,
                 "anchor": {"start_line": anchor.start_line, "end_line": anchor.end_line,
                            "block_sha256": anchor.block_sha256},
                 "role": "companion"}
                for anchor in sorted(self.companions)
            ],
            "phase": self.phase,
            "commit_subject": self.commit_subject,
            "acceptance": [
                # Digest the *canonical* criterion text (§2.1 step 4): equivalent criteria
                # collapse to one digest regardless of which raw spelling was retained, so the
                # content digest and idempotency key are invariant to declaration source order.
                {"criterion_id": crit.criterion_id, "text": canonicalize_text(crit.text),
                 "source_anchors": sorted(crit.source_anchors)}
                for crit in sorted(self.acceptance, key=lambda c: c.criterion_id)
            ],
            "dependencies": [
                {"parent": dep.parent, "kind": dep.kind,
                 "payload": _canonical_payload(dict(dep.payload)),
                 "source_anchors": list(dep.all_source_anchors())}
                for dep in sorted(self.dependencies, key=lambda d: (
                    d.parent, d.kind, _canonical_json(_canonical_payload(dict(d.payload)))))
            ],
            "source_anchors": [anchor.as_dict() for anchor in sorted(self.source_anchors)],
        }


@dataclass
class ReconcileResult:
    records: dict[str, SliceRecord]
    errors: list[Diagnostic]
    warnings: list[Diagnostic]
    series: dict[str, tuple[str, ...]] = field(default_factory=dict)

    @property
    def dispatchable(self) -> bool:
        return not self.errors


def canonicalize_text(text: str) -> str:
    """NFC + LF + trimmed lines + folded intra-line whitespace (§2.1 step 4)."""
    normalized = unicodedata.normalize("NFC", text).replace("\r\n", "\n").replace("\r", "\n")
    lines = [re.sub(r"[^\S\n]+", " ", line).strip() for line in normalized.split("\n")]
    return "\n".join(lines).strip()


def criterion_digest(text: str) -> str:
    return hashlib.sha256(canonicalize_text(text).encode("utf-8")).hexdigest()


def _canonical_json(obj: object) -> str:
    """Serialize JSON-compatible I-JSON data according to RFC 8785 (JCS)."""
    if obj is None:
        return "null"
    if obj is True:
        return "true"
    if obj is False:
        return "false"
    if isinstance(obj, int):
        try:
            binary64 = float(obj)
        except OverflowError as exc:
            raise ValueError("JCS integers must be exactly representable as binary64") from exc
        if not math.isfinite(binary64) or int(binary64) != obj:
            raise ValueError("JCS integers must be exactly representable as binary64")
        return _canonical_number(binary64)
    if isinstance(obj, float):
        return _canonical_number(obj)
    if isinstance(obj, str):
        try:
            obj.encode("utf-8")
        except UnicodeEncodeError as exc:
            raise ValueError("JCS strings may not contain lone surrogates") from exc
        return json.dumps(obj, ensure_ascii=False, separators=(",", ":"))
    if isinstance(obj, (list, tuple)):
        return "[" + ",".join(_canonical_json(value) for value in obj) + "]"
    if isinstance(obj, dict):
        if not all(isinstance(key, str) for key in obj):
            raise ValueError("JCS object keys must be strings")
        keys = sorted(obj, key=lambda key: key.encode("utf-16-be", "surrogatepass"))
        return "{" + ",".join(
            f"{_canonical_json(key)}:{_canonical_json(obj[key])}" for key in keys
        ) + "}"
    raise ValueError(f"value of type {type(obj).__name__} is not JSON-compatible")


def _canonical_number(value: float) -> str:
    """Render a finite binary64 using ECMAScript's JSON number spelling."""
    if not math.isfinite(value):
        raise ValueError("JCS numbers must be finite")
    if value == 0:
        return "0"
    sign = "-" if value < 0 else ""
    mantissa, marker, exponent_text = repr(abs(value)).lower().partition("e")
    exponent = int(exponent_text) if marker else 0
    integer, _, fraction = mantissa.partition(".")
    combined = integer + fraction
    leading_zeroes = len(combined) - len(combined.lstrip("0"))
    digits = combined.lstrip("0")
    if fraction:
        digits = digits.rstrip("0")
    decimal_position = len(integer) - leading_zeroes + exponent
    scientific_exponent = decimal_position - 1
    if -6 <= scientific_exponent < 21:
        if decimal_position <= 0:
            body = "0." + "0" * (-decimal_position) + digits
        elif decimal_position >= len(digits):
            body = digits + "0" * (decimal_position - len(digits))
        else:
            body = digits[:decimal_position] + "." + digits[decimal_position:]
        return sign + body
    tail = digits[1:]
    exponent_out = f"+{scientific_exponent}" if scientific_exponent >= 0 else str(scientific_exponent)
    return f"{sign}{digits[0]}{'.' + tail if tail else ''}e{exponent_out}"


def content_digest(body: dict[str, object]) -> str:
    return "sha256:" + hashlib.sha256(_canonical_json(body).encode("utf-8")).hexdigest()


def idempotency_key(normalized_id: str, digest: str) -> str:
    return f"v2-slice-owner/v1:{urllib.parse.quote(normalized_id, safe='')}:{digest}"


def _canonical_rfc3339(value: str) -> str:
    """Parse RFC 3339, reject leap seconds, and render canonical UTC."""
    match = re.fullmatch(
        r"(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(\.\d+)?(Z|[+-]\d{2}:\d{2})", value)
    if match is None or match.group(6) == "60":
        raise ValueError("invalid RFC 3339 timestamp or unsupported leap second")
    offset = match.group(8)
    if offset != "Z" and (int(offset[1:3]) > 23 or int(offset[4:6]) > 59):
        raise ValueError("RFC 3339 offset is out of range")
    parsed = datetime.fromisoformat(value[:-1] + "+00:00" if offset == "Z" else value)
    utc = parsed.astimezone(timezone.utc)
    fraction = f".{utc.microsecond:06d}".rstrip("0") if utc.microsecond else ""
    return utc.strftime("%Y-%m-%dT%H:%M:%S") + fraction + "Z"


def _canonical_payload(payload: dict[str, object]) -> dict[str, object]:
    result = dict(payload)
    if "allowed" in result and isinstance(result["allowed"], (list, tuple)):
        result["allowed"] = sorted(result["allowed"], key=_canonical_json)
    if "not_before" in result and isinstance(result["not_before"], str):
        result["not_before"] = _canonical_rfc3339(result["not_before"])
    return result


def _nonempty_id(value: object) -> bool:
    return isinstance(value, str) and bool(value) and value == value.strip()


def _criterion_error(criterion: Criterion, companion_count: int) -> tuple[str, str] | None:
    """Return a typed diagnostic code/rule for malformed acceptance provenance."""
    if not _nonempty_id(criterion.criterion_id):
        return "conflicting_field", "acceptance criterion ID must be a non-empty canonical string"
    if not isinstance(criterion.text, str) or not canonicalize_text(criterion.text):
        return "conflicting_field", "acceptance criterion text must be non-empty after normalization"
    anchors = criterion.source_anchors
    if not isinstance(anchors, tuple) or not anchors:
        return "source_anchor_mismatch", "acceptance criterion requires a canonical source anchor"
    if any(not isinstance(anchor, str) for anchor in anchors) or len(set(anchors)) != len(anchors):
        return "source_anchor_mismatch", "acceptance source anchors must be unique canonical strings"
    for anchor in anchors:
        if anchor == "owner":
            continue
        match = re.fullmatch(r"companions\[(0|[1-9][0-9]*)\]", anchor)
        if match is None or int(match.group(1)) >= companion_count:
            return (
                "source_anchor_mismatch",
                "acceptance provenance must resolve to owner or an indexed companion",
            )
    return None


def _typed_object(value: object, fields: frozenset[str]) -> bool:
    return isinstance(value, dict) and set(value) == fields and all(_nonempty_id(v) for v in value.values())


def _typed_set(value: object, fields: frozenset[str] | None = None) -> bool:
    if not isinstance(value, (list, tuple)) or not value:
        return False
    valid = all(_typed_object(v, fields) for v in value) if fields else all(_nonempty_id(v) for v in value)
    return valid and len({_canonical_json(v) for v in value}) == len(value)


def _validate_payload(dep: Dependency) -> str | None:
    """Validate the exact serialized plan-24 §4.4 payload union."""
    if not isinstance(dep.payload, tuple):
        return "payload must be an ordered tuple of field/value pairs"
    if any(not isinstance(pair, tuple) or len(pair) != 2 or not isinstance(pair[0], str)
           for pair in dep.payload):
        return "payload must contain string-named field/value pairs"
    fields = [pair[0] for pair in dep.payload]
    if len(fields) != len(set(fields)):
        return "payload contains duplicate field names"
    payload = dict(dep.payload)
    expected = DEPENDENCY_PAYLOAD_FIELDS.get(dep.kind)
    if expected is None:
        return None
    if set(payload) != expected:
        return f"{dep.kind} payload fields must be exactly {sorted(expected)!r}"
    try:
        _canonical_json(payload)
    except (TypeError, ValueError, OverflowError):
        return "payload must be finite RFC 8785 canonical JSON"
    if dep.kind == "requires_artifact" and not _typed_object(payload["artifact_kind"], frozenset({"kind", "schema"})):
        return "artifact_kind must be an ArtifactKindRef"
    if dep.kind == "requires_acceptance" and not _nonempty_id(payload["criterion"]):
        return "criterion must be an AcceptanceCriterionId"
    if dep.kind == "requires_decision":
        if not _nonempty_id(payload["decision"]):
            return "decision must be a TaskDecisionId"
        if not _typed_set(payload["allowed"], frozenset({"registry_code", "schema_version"})):
            return "allowed must be a BTreeSet<DecisionValueV1>"
    if dep.kind == "requires_plan_outcome":
        if not _nonempty_id(payload["child_plan"]):
            return "child_plan must be a PlanId"
        if not _typed_set(payload["allowed"]):
            return "allowed must be a BTreeSet<OutcomeClassV1>"
    if dep.kind == "requires_terminal" and not _typed_set(payload["allowed"]):
        return "allowed must be an explicit terminal set"
    if dep.kind == "not_before":
        try:
            _canonical_rfc3339(payload["not_before"])
        except (TypeError, ValueError):
            return "not_before must be valid RFC 3339; leap seconds unsupported"
    return None


def _merge_acceptance(record: SliceRecord, section: Section, is_owner: bool,
                      warnings: list[Diagnostic], errors: list[Diagnostic]) -> None:
    for crit in section.acceptance:
        malformed = _criterion_error(crit, len(record.companions))
        if malformed is not None:
            code, rule = malformed
            errors.append(_error(code, section.anchor, repr(crit), rule, record.normalized_id))
            continue
        digest = criterion_digest(crit.text)
        same_id = next((c for c in record.acceptance if c.criterion_id == crit.criterion_id), None)
        if same_id is not None and criterion_digest(same_id.text) != digest:
            errors.append(_error("conflicting_field", section.anchor, crit.text,
                                 f"criterion {crit.criterion_id} has contradictory text",
                                 record.normalized_id))
            continue
        equivalent = next((c for c in record.acceptance if criterion_digest(c.text) == digest), None)
        if equivalent is not None:
            merged = tuple(sorted(set(equivalent.source_anchors) | set(crit.source_anchors)))
            record.acceptance[record.acceptance.index(equivalent)] = Criterion(
                equivalent.criterion_id, equivalent.text, merged)
            warnings.append(_warning("duplicate_description", section.anchor, crit.text,
                                     "equivalent acceptance text; anchors merged",
                                     record.normalized_id))
        else:
            record.acceptance.append(crit)
            if not is_owner:
                warnings.append(_warning("compatible_companion_addition", section.anchor,
                                         crit.text, "distinct non-conflicting criterion added",
                                         record.normalized_id))


def reconcile(sections: list[Section], authority_keys: frozenset[str] | None = None,
              series: dict[str, tuple[str, ...]] | None = None,
              repo_root: Path | None = None, source_commit: str | None = None,
              indexed_plan_paths: frozenset[str] | None = None) -> ReconcileResult:
    """Reconcile declaring sections into one owner record per normalized scalar ID.

    ``authority_keys`` is the explicit key set from the bootstrap manifest (pre-cutover)
    or the activated canonical graph (post-cutover). When provided, every declaration must
    map to a key and every key must have a declaration (``missing_id``); ``None`` skips the
    authority join (classification-only fixtures). Authoritative validation supplies
    ``repo_root`` plus ``source_commit`` to hash every anchored Git block, and
    ``indexed_plan_paths`` to constrain owner selection to the ordered plan set.
    """
    errors: list[Diagnostic] = []
    warnings: list[Diagnostic] = []
    grouped: dict[str, list[tuple[Section, bool]]] = {}
    series_refs: list[tuple[str, Anchor, str]] = []
    pin_values = (repo_root, source_commit, indexed_plan_paths)
    pin_supplied = tuple(value is not None for value in pin_values)
    pinned_blobs: dict[str, bytes | None] = {}
    pin_anchor = sections[0].anchor if sections else Anchor("(pin-context)", 0, 0, "")
    if any(pin_supplied) and not all(pin_supplied):
        errors.append(_error(
            "source_anchor_mismatch", pin_anchor, repr(pin_supplied),
            "repo_root, source_commit, and indexed_plan_paths are an all-or-none pin context",
        ))
    elif all(pin_supplied):
        assert repo_root is not None and source_commit is not None
        if not COMMIT_OID.fullmatch(source_commit):
            errors.append(_error("source_anchor_mismatch", pin_anchor, source_commit,
                                 "source_commit must be a full lowercase immutable commit OID"))
        else:
            observed = run_git(repo_root, "rev-parse", "--verify", f"{source_commit}^{{commit}}")
            try:
                resolved = observed.stdout.decode("utf-8").strip()
            except UnicodeDecodeError:
                resolved = ""
            if observed.error is not None or observed.returncode != 0:
                resolved = ""
            if resolved != source_commit:
                errors.append(_error(
                    "source_anchor_mismatch", pin_anchor, source_commit,
                    "source_commit does not identify an immutable commit object",
                ))
            else:
                for path in sorted({section.anchor.path for section in sections}):
                    shown = run_git(
                        repo_root, "show", f"{source_commit}:{path}",
                        max_output_bytes=4 * 1024 * 1024,
                    )
                    pinned_blobs[path] = (
                        shown.stdout if shown.error is None and shown.returncode == 0 else None
                    )

    for section in sections:
        errors.extend(validate_source_anchor(section.anchor))
        if pinned_blobs:
            errors.extend(_validate_pinned_anchor(section.anchor,
                                                  pinned_blobs.get(section.anchor.path)))
        classification = classify_token(section.raw_id)
        if section.incidental:
            warnings.append(_warning("incidental_reference", section.anchor, section.raw_id,
                                     "mention is non-dispatchable evidence"))
            continue
        if indexed_plan_paths is not None and section.anchor.path not in indexed_plan_paths:
            errors.append(_error(
                "source_anchor_mismatch", section.anchor, section.anchor.path,
                "non-incidental declaration path is outside the indexed plan set",
            ))
        if classification.kind == "malformed":
            errors.append(_error(classification.code, section.anchor, section.raw_id,
                                 classification.rule, suggestion=classification.suggestion))
            continue
        if classification.kind == "series":
            series_refs.append((classification.series_id, section.anchor, section.raw_id))
            continue
        is_owner = section.role == "owner"
        for normalized_id in classification.ids:
            grouped.setdefault(normalized_id, []).append((section, is_owner))

    records: dict[str, SliceRecord] = {}
    for normalized_id in sorted(grouped):
        entries = grouped[normalized_id]
        owners = [section for section, is_owner in entries if is_owner]
        if authority_keys is not None and normalized_id not in authority_keys:
            first = entries[0][0]
            errors.append(_error("missing_id", first.anchor, first.raw_id,
                                 "declaration has no explicit-authority key", normalized_id))
            continue
        if not owners:
            first = entries[0][0]
            errors.append(_error("missing_owner", first.anchor, first.raw_id,
                                 "no declaring section owns this slice", normalized_id))
            continue
        if len(owners) > 1:
            for extra in owners[1:]:
                errors.append(_error("conflicting_owners", extra.anchor, extra.raw_id,
                                     "more than one owner declared", normalized_id))
            continue
        records[normalized_id] = _build_record(normalized_id, entries, owners[0], warnings, errors)

    if authority_keys is not None:
        for key in sorted(authority_keys - set(records)):
            anchor = Anchor("(authority)", 0, 0, "")
            errors.append(_error("missing_id", anchor, key,
                                 "explicit-authority key has no declaration", key))

    _validate_edges(records, authority_keys, errors)
    _detect_cycles(records, errors)
    _finalize_digests(records, errors)
    _validate_series(series_refs, dict(series or {}), records, authority_keys, errors)

    return ReconcileResult(records, sort_diagnostics(errors), sort_diagnostics(warnings),
                           dict(series or {}))


def _build_record(normalized_id: str, entries: list[tuple[Section, bool]], owner: Section,
                  warnings: list[Diagnostic], errors: list[Diagnostic]) -> SliceRecord:
    record = SliceRecord(normalized_id=normalized_id, owner=owner.anchor,
                         owner_heading=owner.heading)
    if not isinstance(owner.heading, str) or not owner.heading.strip():
        errors.append(_error("missing_owner", owner.anchor, str(owner.heading),
                             "owner requires a non-empty exact declaring heading",
                             normalized_id))
    if not isinstance(owner.phase, int) or not 0 <= owner.phase <= 5:
        errors.append(_error("invalid_phase", owner.anchor, str(owner.phase),
                             "phase must be an integer 0..5", normalized_id))
    else:
        record.phase = owner.phase
    if not owner.commit_subject:
        errors.append(_error("conflicting_field", owner.anchor, "",
                             "owner requires a commit subject", normalized_id))
    else:
        record.commit_subject = owner.commit_subject
    record.source_anchors.append(owner.anchor)

    for section, is_owner in entries:
        if not is_owner:
            record.companions.append(section.anchor)
            record.source_anchors.append(section.anchor)
            if section.phase is not None and section.phase != record.phase:
                errors.append(_error("conflicting_field", section.anchor, str(section.phase),
                                     "companion may not override owner phase", normalized_id))
            if section.commit_subject and section.commit_subject != record.commit_subject:
                errors.append(_error("conflicting_field", section.anchor, section.commit_subject,
                                     "companion may not override owner commit subject",
                                     normalized_id))
    for section, is_owner in entries:
        _merge_acceptance(record, section, is_owner, warnings, errors)
        record.dependencies.extend(section.dependencies)
    return record


def _validate_series(refs: list[tuple[str, Anchor, str]], series_map: dict[str, tuple[str, ...]],
                     records: dict[str, SliceRecord], authority_keys: frozenset[str] | None,
                     errors: list[Diagnostic]) -> None:
    """Fail-closed validation of every referenced series (§2.1 rule 5).

    A series is rejected (``invalid_series``) when it has no declared members, or when any
    member is not a single canonical scalar ID (noncanonical), is itself a series
    (recursive), has no reconciling declaration/authority key (unknown), or is already
    claimed by a different series (conflicting overlap). A series whose members are all
    valid canonical scalars that reconcile against actual records/authority keys, with no
    cross-series overlap, passes.
    """
    known = set(records) if authority_keys is None else set(records) | set(authority_keys)
    claimed_by: dict[str, str] = {}
    for series_id, anchor, raw in refs:
        members = series_map.get(series_id)
        if not members:
            errors.append(_error("invalid_series", anchor, raw,
                                 "series has no declared members", series_id))
            continue
        for member in members:
            member_class = classify_token(member)
            if member_class.kind == "series":
                errors.append(_error("invalid_series", anchor, member,
                                     "a series member may not itself be a series", series_id))
                continue
            if member_class.kind != "declaration" or member_class.ids != (member,):
                errors.append(_error("invalid_series", anchor, member,
                                     "series member is not a single canonical scalar ID",
                                     series_id))
                continue
            if member not in known:
                errors.append(_error("invalid_series", anchor, member,
                                     "series member has no declaration or authority key",
                                     series_id))
                continue
            prior = claimed_by.get(member)
            if prior is not None and prior != series_id:
                errors.append(_error("invalid_series", anchor, member,
                                     f"series member is also claimed by {prior}", series_id))
                continue
            claimed_by[member] = series_id


def _validate_edges(records: dict[str, SliceRecord], authority_keys: frozenset[str] | None,
                    errors: list[Diagnostic]) -> None:
    known = set(records) if authority_keys is None else set(records) | set(authority_keys)
    for normalized_id, record in sorted(records.items()):
        deduped: dict[tuple[str, str, str], Dependency] = {}
        for dep in record.dependencies:
            if dep.kind not in EDGE_KINDS:
                errors.append(_error("invalid_edge_type_or_payload", record.owner, dep.kind,
                                     f"unknown edge kind {dep.kind!r}", normalized_id))
                continue
            payload_rule = _validate_payload(dep)
            if payload_rule:
                errors.append(_error("invalid_edge_type_or_payload", record.owner, dep.kind,
                                     payload_rule, normalized_id))
                continue
            canonical_anchors = {anchor.ref() for anchor in record.source_anchors}
            if not dep.all_source_anchors() or not set(dep.all_source_anchors()) <= canonical_anchors:
                errors.append(_error("source_anchor_mismatch", record.owner, dep.parent,
                                     "dependency provenance must resolve to a pinned owner or companion anchor",
                                     normalized_id))
                continue
            if dep.parent == normalized_id:
                errors.append(_error("invalid_edge_type_or_payload", record.owner, dep.parent,
                                     "a slice may not depend on itself", normalized_id))
                continue
            if dep.parent not in known:
                errors.append(_error("unresolved_dependency", record.owner, dep.parent,
                                     "edge endpoint is not a known scalar ID", normalized_id))
                continue
            try:
                canonical_payload = _canonical_payload(dict(dep.payload))
                payload_json = _canonical_json(canonical_payload)
            except (TypeError, ValueError, OverflowError):
                errors.append(_error("invalid_edge_type_or_payload", record.owner, dep.kind,
                                     "payload must be finite RFC 8785 canonical JSON",
                                     normalized_id))
                continue
            key = (dep.parent, dep.kind, payload_json)
            prior = deduped.get(key)
            anchors = dep.all_source_anchors()
            if prior is None:
                deduped[key] = Dependency(dep.parent, dep.kind,
                                          tuple(canonical_payload.items()),
                                          source_anchors=anchors)
            else:
                deduped[key] = Dependency(
                    prior.parent, prior.kind, prior.payload,
                    source_anchors=tuple(sorted(set(prior.all_source_anchors()) | set(anchors))),
                )
        record.dependencies = [deduped[key] for key in sorted(deduped)]


def _detect_cycles(records: dict[str, SliceRecord], errors: list[Diagnostic]) -> None:
    graph = {
        nid: sorted({dep.parent for dep in rec.dependencies
                     if dep.kind in GATING_KINDS and dep.parent in records})
        for nid, rec in records.items()
    }
    visiting: set[str] = set()
    done: set[str] = set()
    reported: set[frozenset[str]] = set()

    def visit(node: str, trail: list[str]) -> None:
        if node in visiting:
            cycle = trail[trail.index(node):] + [node]
            key = frozenset(cycle)
            if key not in reported:
                reported.add(key)
                errors.append(_error("cycle", records[node].owner, " -> ".join(cycle),
                                     "gating dependencies form a cycle", node))
            return
        if node in done:
            return
        visiting.add(node)
        for parent in graph[node]:
            visit(parent, trail + [node])
        visiting.discard(node)
        done.add(node)

    for node in sorted(graph):
        visit(node, [])


def _finalize_digests(records: dict[str, SliceRecord], errors: list[Diagnostic]) -> None:
    seen_keys: dict[str, str] = {}
    for normalized_id, record in sorted(records.items()):
        digest = content_digest(record.reconciled_body())
        record.content_digest = digest
        record.idempotency_key = idempotency_key(normalized_id, digest)
        if record.idempotency_key in seen_keys:
            errors.append(_error("duplicate_idempotency_key", record.owner, record.idempotency_key,
                                 f"idempotency key collides with {seen_keys[record.idempotency_key]}",
                                 normalized_id))
        else:
            seen_keys[record.idempotency_key] = normalized_id


def source_anchor_observations(records: dict[str, SliceRecord]) -> list[list[str]]:
    """Return reconciled anchor observations; not the canonical plan-tree source set."""
    return [list(pair) for pair in sorted(
        {(anchor.path, anchor.block_sha256)
         for record in records.values() for anchor in record.source_anchors}
    )]


def source_set_digest(observations: list[list[str]]) -> str:
    """Digest one versioned canonical source-set projection supplied by Git observation."""
    return "sha256:" + hashlib.sha256(
        _canonical_json(sorted(observations)).encode("utf-8")
    ).hexdigest()


# ---------------------------------------------------------------------------
# Bootstrap locator (§2.1) — validation only, never dispatch
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class BootstrapFailure:
    reason: str  # multiple_explicit | missing | not_regular | unreadable | outside_root |
    #              unknown_repo | invalid_json | schema_mismatch
    detail: str


def locate_bootstrap_manifest(repo_root: Path, explicit: object = None, env: str | None = None,
                              repo_identity_ok: bool = True) -> tuple[Path | None, BootstrapFailure | None]:
    """Resolve exactly one bootstrap manifest by §2.1 precedence, or a typed failure.

    Precedence: (1) explicit argument, (2) ``TRACEDECAY_V2_EXECUTION_MANIFEST`` value,
    (3) ``<repo-root>/.tracedecay/v2-execution-manifest.json``. No directory/board/profile
    scanning. A non-explicit selection must resolve to a regular file beneath ``repo_root``.
    The candidate must parse as JSON (``invalid_json`` otherwise) and satisfy the manifest
    schema — a top-level object carrying a ``slices`` object whose entries are objects
    (``schema_mismatch`` otherwise); a bare ``{}`` is rejected as a schema mismatch.
    """
    if not repo_identity_ok:
        return None, BootstrapFailure("unknown_repo", "repository identity is unknown")

    explicit_values = [explicit] if isinstance(explicit, (str, Path)) else list(explicit or [])
    if len(explicit_values) > 1:
        return None, BootstrapFailure("multiple_explicit", "more than one explicit manifest given")

    if explicit_values:
        return _validate_manifest(Path(explicit_values[0]), repo_root, contained=False)
    if env is None:
        env = os.environ.get("TRACEDECAY_V2_EXECUTION_MANIFEST")
    if env:
        return _validate_manifest(Path(env), repo_root, contained=False)

    default = repo_root / ".tracedecay" / "v2-execution-manifest.json"
    if default.exists():
        return _validate_manifest(default, repo_root, contained=True)
    active = repo_root / ".tracedecay" / "v2-execution-active.json"
    if active.exists():
        selected, pointer_failure = resolve_active_generation(active, repo_root, "manifest")
        if pointer_failure is not None or selected is None:
            return None, pointer_failure
        return _validate_manifest(selected, repo_root, contained=True)
    return None, BootstrapFailure("missing", "no bootstrap manifest candidate found")


def resolve_active_generation(pointer: Path, repo_root: Path,
                              member: str) -> tuple[Path | None, BootstrapFailure | None]:
    if member not in {"manifest", "state"}:
        return None, BootstrapFailure("schema_mismatch", f"unknown active member {member!r}")
    try:
        raw = pointer.read_bytes()
        def unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
            result: dict[str, object] = {}
            for key, value in pairs:
                if key in result:
                    raise ValueError(f"duplicate JSON object key {key!r}")
                result[key] = value
            return result
        document = json.loads(raw, object_pairs_hook=unique_object)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        return None, BootstrapFailure("invalid_json", f"{pointer} is invalid: {error}")
    expected = {
        "schema", "generation", "manifest", "state", "manifest_sha256", "state_sha256",
    }
    if (
        not isinstance(document, dict)
        or set(document) != expected
        or document.get("schema") != "tracedecay.v2.execution-generation-pointer/v1"
        or not all(isinstance(document.get(key), str) and document[key] for key in expected)
    ):
        return None, BootstrapFailure("schema_mismatch", f"{pointer} has invalid pointer schema")
    candidate = (pointer.parent / document[member]).resolve()
    try:
        candidate.relative_to(repo_root.resolve())
    except ValueError:
        return None, BootstrapFailure("outside_root", f"{pointer} {member} escapes repository root")
    if not candidate.is_file():
        return None, BootstrapFailure("missing", f"active {member} {candidate} does not exist")
    try:
        digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
    except OSError as error:
        return None, BootstrapFailure("unreadable", f"cannot read active {member}: {error}")
    if digest != document[f"{member}_sha256"]:
        return None, BootstrapFailure("schema_mismatch", f"active {member} digest mismatch")
    return candidate, None


def _validate_manifest(path: Path, repo_root: Path, contained: bool) -> tuple[Path | None, BootstrapFailure | None]:
    resolved = path.resolve() if path.exists() else path
    if not resolved.exists():
        return None, BootstrapFailure("missing", f"{path} does not exist")
    if not resolved.is_file():
        return None, BootstrapFailure("not_regular", f"{path} is not a regular file")
    if contained:
        try:
            resolved.relative_to(repo_root.resolve())
        except ValueError:
            return None, BootstrapFailure("outside_root", f"{path} escapes the repository root")
    try:
        raw = resolved.read_bytes()
    except OSError:
        return None, BootstrapFailure("unreadable", f"{path} is not readable")
    failure = _validate_manifest_schema(path, raw)
    if failure is not None:
        return None, failure
    return resolved, None


def _validate_manifest_schema(path: Path, raw: bytes) -> BootstrapFailure | None:
    """Reject a manifest that is not JSON or does not satisfy the §2.1 authority schema."""
    try:
        def unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
            result: dict[str, object] = {}
            for key, value in pairs:
                if key in result:
                    raise ValueError(f"duplicate JSON object key {key!r}")
                result[key] = value
            return result
        document = json.loads(raw, object_pairs_hook=unique_object)
    except (json.JSONDecodeError, UnicodeDecodeError, ValueError) as exc:
        return BootstrapFailure("invalid_json", f"{path} is not valid JSON: {exc}")
    if not isinstance(document, dict):
        return BootstrapFailure("schema_mismatch", f"{path} root must be a JSON object")
    if document.get("schema") != "tracedecay.v2.slice-dag/v1":
        return BootstrapFailure(
            "schema_mismatch",
            f"{path} requires schema 'tracedecay.v2.slice-dag/v1'",
        )
    slices = document.get("slices")
    if not isinstance(slices, dict):
        return BootstrapFailure("schema_mismatch", f"{path} requires a 'slices' object")
    for key, value in slices.items():
        if not key or not isinstance(value, dict):
            return BootstrapFailure(
                "schema_mismatch", f"{path} slice entry {key!r} must map a non-empty ID to an object")
    return None


# ---------------------------------------------------------------------------
# Pre/post-cutover reconciliation (§2.1 step 6) — comparison only
# ---------------------------------------------------------------------------


def reconcile_against_authority(records: dict[str, SliceRecord], authority: dict[str, object],
                                phase: str,
                                source_observations: list[list[str]],
                                canonical_series: dict[str, tuple[str, ...]] | None = None
                                ) -> list[Diagnostic]:
    """Parse and compare one exact authority document, recomputing all integrity fields."""
    diagnostics: list[Diagnostic] = []
    authority_anchor = Anchor(f"({phase}-authority)", 0, 0, "")
    top_keys = {"schema", "graph_revision", "source_set_digest", "slices", "series"}
    graph_revision = authority.get("graph_revision") if isinstance(authority, dict) else None
    if (not isinstance(authority, dict) or set(authority) != top_keys
            or authority.get("schema") != "tracedecay.v2.slice-dag/v1"
            or isinstance(graph_revision, bool)
            or not isinstance(graph_revision, int)
            or graph_revision < 0
            or not isinstance(authority.get("slices"), dict)
            or not isinstance(authority.get("series"), dict)):
        return [_error("reconciliation_mismatch", authority_anchor, repr(authority),
                       f"{phase} authority does not match the exact top-level schema")]

    authority_slices = authority["slices"]
    candidate_ids = set(records)
    authority_ids = set(authority_slices)

    canonical_series = canonical_series or {}
    malformed_series = any(
        classify_token(key).kind != "series" or not isinstance(members, tuple)
        or list(members) != sorted(set(members))
        or not set(members) <= set(records)
        or any(classify_token(member).ids != (member,) for member in members)
        for key, members in canonical_series.items()
    )
    expected_series = {key: list(members) for key, members in sorted(canonical_series.items())}
    if malformed_series:
        diagnostics.append(_error("invalid_series", authority_anchor, repr(canonical_series),
                                  "canonical series requires exact IDs and sorted unique scalar members"))
    elif authority["series"] != expected_series:
        diagnostics.append(_error("reconciliation_mismatch", authority_anchor,
                                  repr(authority["series"]),
                                  f"series membership differs from canonical {phase} series"))

    expected_source_digest = source_set_digest(source_observations)
    if authority.get("source_set_digest") != expected_source_digest:
        diagnostics.append(_error("digest_mismatch", authority_anchor,
                                  str(authority.get("source_set_digest")),
                                  f"source_set_digest differs from canonical {phase} body"))

    for extra in sorted(candidate_ids - authority_ids):
        diagnostics.append(_error("reconciliation_mismatch", records[extra].owner, extra,
                                  f"candidate slice absent from {phase} authority", extra))
    for missing in sorted(authority_ids - candidate_ids):
        diagnostics.append(_error("reconciliation_mismatch", authority_anchor, missing,
                                  f"{phase} authority slice absent from candidate", missing))
    for normalized_id in sorted(candidate_ids & authority_ids):
        record = records[normalized_id]
        expected = authority_slices[normalized_id]
        body_keys = set(record.reconciled_body())
        if not isinstance(expected, dict) or set(expected) != body_keys | {
                "content_digest", "idempotency_key"}:
            diagnostics.append(_error("reconciliation_mismatch", record.owner, normalized_id,
                                      f"{phase} authority slice has a malformed exact schema",
                                      normalized_id))
            continue
        authority_body = {key: expected[key] for key in body_keys}
        acceptance = authority_body["acceptance"]
        companions = authority_body["companions"]
        if not isinstance(acceptance, list) or not isinstance(companions, list):
            diagnostics.append(_error("reconciliation_mismatch", record.owner, normalized_id,
                                      f"{phase} authority contains malformed acceptance criteria",
                                      normalized_id))
            continue
        malformed_acceptance = False
        for raw_criterion in acceptance:
            if (not isinstance(raw_criterion, dict)
                    or set(raw_criterion) != {"criterion_id", "text", "source_anchors"}
                    or not isinstance(raw_criterion.get("source_anchors"), list)):
                diagnostics.append(_error(
                    "conflicting_field", record.owner, repr(raw_criterion),
                    "authority acceptance criterion must match the exact typed schema",
                    normalized_id,
                ))
                malformed_acceptance = True
                continue
            criterion = Criterion(
                raw_criterion["criterion_id"], raw_criterion["text"],
                tuple(raw_criterion["source_anchors"]),
            )
            malformed = _criterion_error(criterion, len(record.companions))
            if malformed is not None:
                code, rule = malformed
                diagnostics.append(_error(code, record.owner, repr(raw_criterion), rule,
                                          normalized_id))
                malformed_acceptance = True
        if malformed_acceptance:
            continue
        if not _authority_edges_valid(
                authority_body["dependencies"],
                {anchor.ref() for anchor in record.source_anchors}):
            diagnostics.append(_error("reconciliation_mismatch", record.owner, normalized_id,
                                      f"{phase} authority contains a malformed dependency",
                                      normalized_id))
            continue
        authority_body["dependencies"] = sorted([
            {**edge, "payload": _canonical_payload(edge.get("payload", {})),
             "source_anchors": sorted(set(edge["source_anchors"]))}
            for edge in authority_body["dependencies"]
        ], key=lambda edge: (edge["parent"], edge["kind"],
                             _canonical_json(edge["payload"])))
        try:
            recomputed_digest = content_digest(authority_body)
        except (TypeError, ValueError, OverflowError):
            diagnostics.append(_error("digest_mismatch", record.owner, normalized_id,
                                      f"{phase} authority body is not canonical I-JSON",
                                      normalized_id))
            continue
        if expected["content_digest"] != recomputed_digest:
            diagnostics.append(_error("digest_mismatch", record.owner,
                                      str(expected["content_digest"]),
                                      f"content_digest does not match canonical {phase} body",
                                      normalized_id))
        recomputed_key = idempotency_key(normalized_id, recomputed_digest)
        if expected["idempotency_key"] != recomputed_key:
            diagnostics.append(_error("idempotency_mismatch", record.owner,
                                      str(expected["idempotency_key"]),
                                      f"idempotency_key does not match canonical {phase} body",
                                      normalized_id))
        if authority_body != record.reconciled_body():
            diagnostics.append(_error("reconciliation_mismatch", record.owner, normalized_id,
                                      f"canonical body differs from {phase} authority",
                                      normalized_id))
    return sort_diagnostics(diagnostics)


def _authority_edges_valid(edges: object, canonical_anchors: set[str]) -> bool:
    """Validate the typed authority edge projection without unused set machinery."""
    if not isinstance(edges, list):
        return False
    for edge in edges:
        if (isinstance(edge, dict) and edge.get("kind") in PAYLOAD_FREE_KINDS
                and "payload" not in edge):
            edge = {**edge, "payload": {}}
        if (not isinstance(edge, dict)
                or set(edge) != {"parent", "kind", "payload", "source_anchors"}
                or not isinstance(edge["parent"], str)
                or not isinstance(edge["kind"], str)
                or not isinstance(edge["payload"], dict)
                or not isinstance(edge["source_anchors"], list)
                or not edge["source_anchors"]
                or not all(isinstance(value, str) and value for value in edge["source_anchors"])
                or not set(edge["source_anchors"]) <= canonical_anchors):
            return False
        dependency = Dependency(
            edge["parent"], edge["kind"], tuple(edge["payload"].items()),
            source_anchors=tuple(edge["source_anchors"]),
        )
        if edge["kind"] not in EDGE_KINDS or _validate_payload(dependency) is not None:
            return False
    return True


if __name__ == "__main__":  # pragma: no cover - module is a library, not a CLI
    raise SystemExit("slice_authority is a validation library; use plan_inventory.py for the CLI")
