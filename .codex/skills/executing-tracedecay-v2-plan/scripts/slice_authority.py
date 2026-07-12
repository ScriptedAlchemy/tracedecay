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
import re
import unicodedata
import urllib.parse
from dataclasses import dataclass, field
from pathlib import Path

EN_DASH = "–"

# A canonical block hash is the lowercase hex SHA-256 of the anchored source block.
SHA256_HEX = re.compile(r"^[0-9a-f]{64}$")

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
            "owner": self.owner.as_dict(),
            "companions": [anchor.as_dict() for anchor in sorted(self.companions)],
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
                {"parent": dep.parent, "kind": dep.kind, "payload": dict(dep.payload)}
                for dep in sorted(self.dependencies, key=lambda d: (d.parent, d.kind))
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
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def content_digest(body: dict[str, object]) -> str:
    return "sha256:" + hashlib.sha256(_canonical_json(body).encode("utf-8")).hexdigest()


def idempotency_key(normalized_id: str, digest: str) -> str:
    return f"v2-slice-owner/v1:{urllib.parse.quote(normalized_id, safe='')}:{digest}"


def _validate_payload(dep: Dependency) -> str | None:
    payload = dict(dep.payload)
    required = {
        "requires_artifact": "artifact",
        "requires_acceptance": "acceptance",
        "requires_decision": "decision",
        "requires_plan_outcome": "plan_outcome",
        "requires_terminal": "terminal_set",
        "not_before": "timestamp",
    }.get(dep.kind)
    if required and not payload.get(required):
        return f"{dep.kind} edge requires a {required} payload"
    if dep.kind == "requires_success" and payload:
        return "requires_success carries no payload"
    return None


def _merge_acceptance(record: SliceRecord, section: Section, is_owner: bool,
                      warnings: list[Diagnostic], errors: list[Diagnostic]) -> None:
    for crit in section.acceptance:
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
              series: dict[str, tuple[str, ...]] | None = None) -> ReconcileResult:
    """Reconcile declaring sections into one owner record per normalized scalar ID.

    ``authority_keys`` is the explicit key set from the bootstrap manifest (pre-cutover)
    or the activated canonical graph (post-cutover). When provided, every declaration must
    map to a key and every key must have a declaration (``missing_id``); ``None`` skips the
    authority join (classification-only fixtures).
    """
    errors: list[Diagnostic] = []
    warnings: list[Diagnostic] = []
    grouped: dict[str, list[tuple[Section, bool]]] = {}
    series_refs: list[tuple[str, Anchor, str]] = []

    for section in sections:
        errors.extend(validate_source_anchor(section.anchor))
        classification = classify_token(section.raw_id)
        if section.incidental:
            warnings.append(_warning("incidental_reference", section.anchor, section.raw_id,
                                     "mention is non-dispatchable evidence"))
            continue
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
    record = SliceRecord(normalized_id=normalized_id, owner=owner.anchor)
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
        deduped: list[Dependency] = []
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
            if dep.parent == normalized_id:
                errors.append(_error("invalid_edge_type_or_payload", record.owner, dep.parent,
                                     "a slice may not depend on itself", normalized_id))
                continue
            if dep.parent not in known:
                errors.append(_error("unresolved_dependency", record.owner, dep.parent,
                                     "edge endpoint is not a known scalar ID", normalized_id))
                continue
            if dep not in deduped:
                deduped.append(dep)
        record.dependencies = deduped


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


def source_set_digest(records: dict[str, SliceRecord]) -> str:
    pairs = sorted(
        {(anchor.path, anchor.block_sha256)
         for record in records.values() for anchor in record.source_anchors}
    )
    return "sha256:" + hashlib.sha256(_canonical_json(pairs).encode("utf-8")).hexdigest()


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
    if env:
        return _validate_manifest(Path(env), repo_root, contained=False)

    default = repo_root / ".tracedecay" / "v2-execution-manifest.json"
    if not default.exists():
        return None, BootstrapFailure("missing", "no bootstrap manifest candidate found")
    return _validate_manifest(default, repo_root, contained=True)


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
        document = json.loads(raw)
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
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
                                phase: str) -> list[Diagnostic]:
    """Compare reconciled candidate records to bootstrap (pre) or graph (post) authority.

    Emits ``reconciliation_mismatch`` diagnostics for extra/missing IDs, edge differences,
    and per-owner digest drift. Empty result means the candidate matches the authority and
    an atomic activation receipt may be recorded by the (out-of-scope) executor.
    """
    diagnostics: list[Diagnostic] = []
    authority_slices = authority.get("slices", {})
    candidate_ids = set(records)
    authority_ids = set(authority_slices)

    for extra in sorted(candidate_ids - authority_ids):
        diagnostics.append(_error("reconciliation_mismatch", records[extra].owner, extra,
                                  f"candidate slice absent from {phase} authority", extra))
    for missing in sorted(authority_ids - candidate_ids):
        anchor = Anchor(f"({phase}-authority)", 0, 0, "")
        diagnostics.append(_error("reconciliation_mismatch", anchor, missing,
                                  f"{phase} authority slice absent from candidate", missing))
    for normalized_id in sorted(candidate_ids & authority_ids):
        record = records[normalized_id]
        expected = authority_slices[normalized_id]
        expected_digest = expected.get("content_digest") if isinstance(expected, dict) else None
        if expected_digest is not None and expected_digest != record.content_digest:
            diagnostics.append(_error("reconciliation_mismatch", record.owner,
                                      record.content_digest,
                                      f"content digest differs from {phase} authority",
                                      normalized_id))
        expected_edges = _edge_set(expected.get("dependencies", []) if isinstance(expected, dict) else [])
        candidate_edges = _edge_set(
            [{"parent": dep.parent, "kind": dep.kind} for dep in record.dependencies])
        if expected_edges != candidate_edges:
            diagnostics.append(_error("reconciliation_mismatch", record.owner, normalized_id,
                                      f"dependency edges differ from {phase} authority",
                                      normalized_id))
    return sort_diagnostics(diagnostics)


def _edge_set(edges: object) -> frozenset[tuple[str, str]]:
    result = set()
    for edge in edges or []:
        if isinstance(edge, dict) and "parent" in edge and "kind" in edge:
            result.add((str(edge["parent"]), str(edge["kind"])))
    return frozenset(result)


if __name__ == "__main__":  # pragma: no cover - module is a library, not a CLI
    raise SystemExit("slice_authority is a validation library; use plan_inventory.py for the CLI")
