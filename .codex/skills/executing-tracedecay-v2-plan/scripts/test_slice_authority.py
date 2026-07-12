#!/usr/bin/env python3
"""Deterministic contract tests for the V2 slice-authority validator.

Each test pins one clause of plan 00 §2.1 (``docs/plans/tracedecay-v2/00-plan-set-index.md``)
with both a positive and a negative case: normalization rules 1-5, owner/companion
reconciliation, typed dependency edges and payloads, whole-graph cycles, canonical digests
and stable idempotency keys, the explicit-authority join, series membership, the bootstrap
locator, and the pre/post-cutover reconciliation gate. The module is a read-only validation
projection; these tests never dispatch, mutate, or assert real source locations.
"""

from __future__ import annotations

import hashlib
import os
import subprocess
import tempfile
import unittest
from unittest import mock
from pathlib import Path

import slice_authority as sa


def _sha(raw: str) -> str:
    """A deterministic 64-char lowercase SHA-256 hex digest (stable across PYTHONHASHSEED)."""
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


def A(path: str, start_line: int, end_line: int, block_sha256: str) -> sa.Anchor:
    """Build a valid compact fixture anchor from a terse stable label."""
    digest = block_sha256 if sa.SHA256_HEX.fullmatch(block_sha256) else _sha(block_sha256)
    return sa.Anchor(path, start_line, end_line, digest)


def _owner(raw: str, *, phase: int = 0, subject: str = "feat: x", **kw) -> sa.Section:
    """A minimal well-formed owner section with a deterministic anchor per raw token.

    The anchor's line and block hash derive from a SHA-256 of ``raw`` — never ``hash()`` —
    so digests and idempotency keys are reproducible regardless of PYTHONHASHSEED.
    """
    digest = _sha(raw)
    line = int(digest[:8], 16) % 9000 + 1
    return sa.Section(raw, "owner", A(f"docs/plans/{raw}.md", line, line + 5, digest),
                      phase=phase, commit_subject=subject, **kw)


# ---------------------------------------------------------------------------
# Rule 1 — simple and compound scalars
# ---------------------------------------------------------------------------


class ScalarClassificationTests(unittest.TestCase):
    def test_simple_scalars_and_suffixes(self) -> None:
        for raw, expected in [
            ("PR 35", "PR 35"),
            ("pr 24e", "PR 24E"),
            ("PR 30B2", "PR 30B2"),
            ("PR 24E0", "PR 24E0"),
        ]:
            got = sa.classify_token(raw)
            self.assertEqual(got.kind, "declaration", raw)
            self.assertEqual(got.ids, (expected,), raw)

    def test_normative_compound_scalars_are_not_ranges(self) -> None:
        ids = [
            "22F-LE", "22F-LS",
            "24D-API1", "24D-API2", "24D-API3", "24D-API4",
            "24D-SDK1", "24D-SDK2", "24D-SDK3",
            "24E-API5", "33S-2",
        ]
        for raw, expected in [("pr 22f-le", "PR 22F-LE")] + [
            (f"PR {slice_id}", f"PR {slice_id}") for slice_id in ids[1:]
        ]:
            got = sa.classify_token(raw)
            self.assertEqual(got.kind, "declaration", raw)
            self.assertEqual(got.ids, (expected,), raw)

    def test_compound_malformations(self) -> None:
        # empty component, zero component, doubled hyphen, non-canonical decimal tail,
        # suffixless base, and a three-part token that is neither range nor multi-component.
        for raw in ["PR 22F-", "PR 22F-0", "PR 22F--LE", "PR 24D-API01",
                    "PR 35-API1", "PR 33S-2-4"]:
            got = sa.classify_token(raw)
            self.assertEqual(got.kind, "malformed", raw)
            self.assertEqual(got.code, "malformed_id", raw)

    def test_compound_malformations_report_the_precise_rule(self) -> None:
        cases = {
            "PR 22F-": "component must",
            "PR 22F-0": "component must",
            "PR 22F--LE": "exactly one identity-bearing hyphen",
            "PR 24D-API01": "component must",
            "PR 35-API1": "non-empty letter-led suffix",
            "PR 33S-2-4": "exactly one identity-bearing hyphen",
            "PR 24D-API1-24D-API4": "exactly one identity-bearing hyphen",
            "PR 24D-API1–4": "same-shape range",
        }
        for raw, expected_rule in cases.items():
            got = sa.classify_token(raw)
            self.assertEqual(got.code, "malformed_id", raw)
            assert got.rule is not None
            self.assertIn(expected_rule, got.rule, raw)

    def test_leading_zero_is_forbidden(self) -> None:
        self.assertEqual(sa.classify_token("PR 007").kind, "malformed")

    def test_pr_prefix_and_whitespace_are_stripped_case_insensitively(self) -> None:
        for raw in ["PR 35", "pr   35", "  Pr 35 ", "35"]:
            self.assertEqual(sa.classify_token(raw).ids, ("PR 35",), raw)

    def test_empty_declaration_is_malformed(self) -> None:
        got = sa.classify_token("PR ")
        self.assertEqual(got.kind, "malformed")
        self.assertEqual(got.code, "malformed_id")


# ---------------------------------------------------------------------------
# Rule 2 — slash lists and alternate spellings
# ---------------------------------------------------------------------------


class SlashClassificationTests(unittest.TestCase):
    def test_slash_between_ids_is_a_multi_id_list(self) -> None:
        self.assertEqual(sa.classify_token("PR 28A/28B").ids, ("PR 28A", "PR 28B"))

    def test_slash_between_stem_and_suffix_is_alternate_spelling(self) -> None:
        self.assertEqual(sa.classify_token("PR 28/A").ids, ("PR 28A",))

    def test_slash_list_dedupes_repeated_members(self) -> None:
        self.assertEqual(sa.classify_token("PR 28A/28A").ids, ("PR 28A",))

    def test_empty_or_invalid_slash_member_is_malformed(self) -> None:
        for raw in ["PR 22F-le/", "PR 28A//28B", "PR /28A"]:
            got = sa.classify_token(raw)
            self.assertEqual(got.kind, "malformed", raw)
            self.assertEqual(got.code, "malformed_id", raw)


# ---------------------------------------------------------------------------
# Rule 3 — dotted suffixes
# ---------------------------------------------------------------------------


class DottedClassificationTests(unittest.TestCase):
    def test_dotted_letter_suffix_is_alternate_spelling(self) -> None:
        self.assertEqual(sa.classify_token("PR 4.E").ids, ("PR 4E",))

    def test_dotted_numeric_suffix_is_identity_bearing(self) -> None:
        self.assertEqual(sa.classify_token("PR 12.1").ids, ("PR 12.1",))

    def test_multiple_dots_and_mixed_suffixes_are_malformed(self) -> None:
        for raw in ["PR 12.1.2", "PR 4.E1", "PR 4.1E"]:
            self.assertEqual(sa.classify_token(raw).kind, "malformed", raw)


# ---------------------------------------------------------------------------
# Rule 4 — ranges (legacy ASCII, en-dash, precedence, rejects)
# ---------------------------------------------------------------------------


class RangeClassificationTests(unittest.TestCase):
    def test_legacy_ascii_ranges(self) -> None:
        self.assertEqual(sa.classify_token("PR 35-37").ids, ("PR 35", "PR 36", "PR 37"))
        self.assertEqual(sa.classify_token("PR 31A-31C").ids,
                         ("PR 31A", "PR 31B", "PR 31C"))

    def test_en_dash_ranges_match_ascii_expansion(self) -> None:
        self.assertEqual(sa.classify_token("PR 35–37").ids,
                         ("PR 35", "PR 36", "PR 37"))
        self.assertEqual(sa.classify_token("PR 31A–31C").ids,
                         ("PR 31A", "PR 31B", "PR 31C"))

    def test_numeric_tail_range_precedence_over_compound(self) -> None:
        # 24E0-24E2 is a legacy numeric-tail range, never a decimal compound component.
        self.assertEqual(sa.classify_token("PR 24E0-24E2").ids,
                         ("PR 24E0", "PR 24E1", "PR 24E2"))

    def test_en_dash_compound_range_varies_numeric_tail(self) -> None:
        self.assertEqual(
            sa.classify_token("PR 24D-API1–24D-API4").ids,
            ("PR 24D-API1", "PR 24D-API2", "PR 24D-API3", "PR 24D-API4"),
        )

    def test_ascii_compound_range_and_abbreviated_endash_are_malformed(self) -> None:
        for raw in ["PR 24D-API1-24D-API4", "PR 24D-API1–4"]:
            got = sa.classify_token(raw)
            self.assertEqual(got.kind, "malformed", raw)
            self.assertEqual(got.code, "malformed_id", raw)

    def test_descending_oversized_and_mixed_stem_ranges_are_rejected(self) -> None:
        self.assertEqual(sa.classify_token("PR 37-35").kind, "malformed")   # descending
        self.assertEqual(sa.classify_token("PR 1-2000").kind, "malformed")  # >1000 members
        self.assertEqual(sa.classify_token("PR 31A-32C").kind, "malformed")  # mixed stem
        self.assertEqual(sa.classify_token("PR 37–35").kind, "malformed")  # endash desc
        self.assertEqual(sa.classify_token("PR 31A–32C").kind, "malformed")  # endash mixed

    def test_boundary_ranges_of_one_member(self) -> None:
        self.assertEqual(sa.classify_token("PR 35-35").ids, ("PR 35",))


# ---------------------------------------------------------------------------
# Rule 5 — series declarations
# ---------------------------------------------------------------------------


class SeriesTests(unittest.TestCase):
    def test_series_heading_classifies_as_series(self) -> None:
        got = sa.classify_token("PR 13 series")
        self.assertEqual(got.kind, "series")
        self.assertEqual(got.series_id, "PR 13 series")

    def test_series_without_declared_members_is_invalid(self) -> None:
        section = sa.Section("PR 13 series", "companion", A("p", 1, 2, "s"))
        result = sa.reconcile([section])
        self.assertEqual([e.code for e in result.errors], ["invalid_series"])
        self.assertEqual(result.errors[0].normalized_id, "PR 13 series")

    def test_series_with_declared_members_reconciling_against_records_passes(self) -> None:
        # Valid declared scalar members must reconcile against actual owner records.
        section = sa.Section("PR 13 series", "companion", A("p", 1, 2, "s"))
        result = sa.reconcile([section, _owner("PR 13A"), _owner("PR 13B")],
                              series={"PR 13 series": ("PR 13A", "PR 13B")})
        self.assertEqual(result.errors, [])
        self.assertEqual(result.series["PR 13 series"], ("PR 13A", "PR 13B"))

    def test_series_members_may_reconcile_against_authority_keys(self) -> None:
        # A member with no owner record but a matching authority key still reconciles.
        section = sa.Section("PR 13 series", "companion", A("p", 1, 2, "s"))
        result = sa.reconcile([section, _owner("PR 13A"), _owner("PR 13B")],
                              authority_keys=frozenset({"PR 13A", "PR 13B"}),
                              series={"PR 13 series": ("PR 13A", "PR 13B")})
        self.assertEqual([e for e in result.errors if e.code == "invalid_series"], [])

    def test_unknown_series_member_fails_closed(self) -> None:
        section = sa.Section("PR 13 series", "companion", A("p", 1, 2, "s"))
        result = sa.reconcile([section, _owner("PR 13A")],
                              series={"PR 13 series": ("PR 13A", "PR 99")})
        self.assertEqual([e.code for e in result.errors], ["invalid_series"])
        self.assertEqual(result.errors[0].raw_value, "PR 99")
        self.assertIn("no declaration or authority key", result.errors[0].violated_rule)

    def test_recursive_series_member_fails_closed(self) -> None:
        section = sa.Section("PR 13 series", "companion", A("p", 1, 2, "s"))
        result = sa.reconcile([section, _owner("PR 13A")],
                              series={"PR 13 series": ("PR 13A", "PR 14 series")})
        self.assertEqual([e.code for e in result.errors], ["invalid_series"])
        self.assertIn("may not itself be a series", result.errors[0].violated_rule)

    def test_noncanonical_series_member_fails_closed(self) -> None:
        section = sa.Section("PR 13 series", "companion", A("p", 1, 2, "s"))
        # Lowercase spelling and a leading-zero token are both noncanonical members.
        for member in ["pr 13a", "PR 007", "PR 13A/13B"]:
            result = sa.reconcile([section, _owner("PR 13A")],
                                  series={"PR 13 series": ("PR 13A", member)})
            self.assertEqual([e.code for e in result.errors], ["invalid_series"], member)
            self.assertIn("single canonical scalar ID", result.errors[0].violated_rule, member)

    def test_conflicting_series_overlap_fails_closed(self) -> None:
        # A member claimed by two different series is a conflicting overlap.
        first = sa.Section("PR 13 series", "companion", A("p", 1, 2, "s"))
        second = sa.Section("PR 14 series", "companion", A("p", 3, 4, "t"))
        result = sa.reconcile([first, second, _owner("PR 13A")],
                              series={"PR 13 series": ("PR 13A",),
                                      "PR 14 series": ("PR 13A",)})
        self.assertEqual([e.code for e in result.errors], ["invalid_series"])
        self.assertIn("also claimed by", result.errors[0].violated_rule)

    def test_malformed_series_stem_is_invalid_series(self) -> None:
        got = sa.classify_token("PR 22F- series")
        self.assertEqual(got.kind, "malformed")
        self.assertEqual(got.code, "invalid_series")


# ---------------------------------------------------------------------------
# Reconciliation: owner selection, companions, acceptance merge, phase/subject
# ---------------------------------------------------------------------------


class ReconciliationTests(unittest.TestCase):
    def test_owner_and_companion_merge_with_equivalent_criteria(self) -> None:
        owner = _owner("PR 4E", acceptance=(
            sa.Criterion("PR-4E-AC-001", "No duplicate owners.", ("owner",)),))
        companion = sa.Section(
            "PR 4E", "companion", A("docs/plans/master.md", 10, 18, "m"),
            acceptance=(sa.Criterion("PR-4E-AC-001", "No   duplicate  owners.",
                                     ("companions[0]",)),))
        result = sa.reconcile([owner, companion])
        self.assertEqual(result.errors, [])
        self.assertEqual([w.code for w in result.warnings], ["duplicate_description"])
        record = result.records["PR 4E"]
        self.assertEqual(record.owner, owner.anchor)
        self.assertEqual(record.companions, [companion.anchor])
        # Equivalent criteria collapse to one with the union of sorted source anchors.
        self.assertEqual(len(record.acceptance), 1)
        self.assertEqual(record.acceptance[0].source_anchors, ("companions[0]", "owner"))

    def test_distinct_companion_criterion_is_compatible_addition(self) -> None:
        owner = _owner("PR 4E", acceptance=(
            sa.Criterion("PR-4E-AC-001", "First.", ("owner",)),))
        companion = sa.Section(
            "PR 4E", "companion", A("docs/plans/master.md", 10, 18, "m"),
            acceptance=(sa.Criterion("PR-4E-AC-002", "Second.", ("companions[0]",)),))
        result = sa.reconcile([owner, companion])
        self.assertEqual([w.code for w in result.warnings],
                         ["compatible_companion_addition"])
        self.assertEqual(len(result.records["PR 4E"].acceptance), 2)

    def test_contradictory_criterion_text_is_conflicting_field(self) -> None:
        owner = _owner("PR 4E", acceptance=(
            sa.Criterion("PR-4E-AC-001", "Reject A.", ("owner",)),))
        companion = sa.Section(
            "PR 4E", "companion", A("docs/plans/master.md", 10, 18, "m"),
            acceptance=(sa.Criterion("PR-4E-AC-001", "Reject B.", ("companions[0]",)),))
        result = sa.reconcile([owner, companion])
        self.assertEqual([e.code for e in result.errors], ["conflicting_field"])

    def test_missing_owner_is_error(self) -> None:
        companion = sa.Section("PR 5", "companion", A("p", 1, 2, "d"),
                               phase=0, commit_subject="c")
        result = sa.reconcile([companion])
        self.assertEqual([e.code for e in result.errors], ["missing_owner"])
        self.assertEqual(result.records, {})

    def test_two_owners_is_conflicting_owners(self) -> None:
        result = sa.reconcile([
            sa.Section("PR 4", "owner", A("p", 1, 2, "a"), phase=0, commit_subject="c"),
            sa.Section("PR 4", "owner", A("p", 3, 4, "b"), phase=0, commit_subject="c"),
        ])
        self.assertEqual([e.code for e in result.errors], ["conflicting_owners"])

    def test_invalid_phase_and_missing_commit_subject(self) -> None:
        bad_phase = sa.Section("PR 3", "owner", A("p", 1, 2, "x"),
                               phase=9, commit_subject="c")
        no_subject = sa.Section("PR 6", "owner", A("p", 3, 4, "y"),
                                phase=0, commit_subject=None)
        result = sa.reconcile([bad_phase, no_subject])
        codes = sorted((e.code, e.normalized_id) for e in result.errors)
        self.assertIn(("invalid_phase", "PR 3"), codes)
        self.assertIn(("conflicting_field", "PR 6"), codes)

    def test_companion_may_not_override_owner_phase_or_subject(self) -> None:
        owner = _owner("PR 4E", phase=1, subject="feat: owner")
        companion = sa.Section("PR 4E", "companion", A("p", 9, 10, "m"),
                               phase=2, commit_subject="feat: companion")
        result = sa.reconcile([owner, companion])
        rules = {e.violated_rule for e in result.errors}
        self.assertIn("companion may not override owner phase", rules)
        self.assertIn("companion may not override owner commit subject", rules)

    def test_incidental_reference_is_warning_only(self) -> None:
        section = sa.Section("PR 9", "owner", A("p", 1, 2, "q"),
                             phase=0, commit_subject="c", incidental=True)
        result = sa.reconcile([section])
        self.assertEqual([w.code for w in result.warnings], ["incidental_reference"])
        self.assertEqual(result.records, {})


# ---------------------------------------------------------------------------
# Digests and idempotency keys
# ---------------------------------------------------------------------------


class DigestTests(unittest.TestCase):
    def _record(self, sections):
        return sa.reconcile(sections)

    def test_digest_and_key_are_invariant_to_declaration_order(self) -> None:
        owner = _owner("PR 4E", acceptance=(
            sa.Criterion("PR-4E-AC-001", "No dup owners.", ("owner",)),))
        companion = sa.Section(
            "PR 4E", "companion", A("docs/plans/master.md", 10, 18, "m"),
            acceptance=(sa.Criterion("PR-4E-AC-001", "No   dup  owners.",
                                     ("companions[0]",)),))
        forward = sa.reconcile([owner, companion]).records["PR 4E"]
        reverse = sa.reconcile([companion, owner]).records["PR 4E"]
        self.assertEqual(forward.content_digest, reverse.content_digest)
        self.assertEqual(forward.idempotency_key, reverse.idempotency_key)

    def test_content_digest_and_idempotency_key_shape(self) -> None:
        record = sa.reconcile([_owner("PR 4E")]).records["PR 4E"]
        self.assertRegex(record.content_digest, r"^sha256:[0-9a-f]{64}$")
        self.assertEqual(
            record.idempotency_key,
            f"v2-slice-owner/v1:PR%204E:{record.content_digest}",
        )

    def test_changed_source_anchor_changes_digest(self) -> None:
        base = sa.reconcile([_owner("PR 4E")]).records["PR 4E"].content_digest
        moved = sa.Section("PR 4E", "owner", A("docs/plans/PR 4E.md", 1, 6, "DIFFERENT"),
                           phase=0, commit_subject="feat: x")
        changed = sa.reconcile([moved]).records["PR 4E"].content_digest
        self.assertNotEqual(base, changed)

    def test_source_set_digest_is_stable_and_hex(self) -> None:
        records = sa.reconcile([_owner("PR 4E"), _owner("PR 4C")]).records
        digest = sa.source_set_digest(records)
        self.assertRegex(digest, r"^sha256:[0-9a-f]{64}$")
        self.assertEqual(digest, sa.source_set_digest(records))

    def test_rfc8785_numbers_strings_and_utf16_key_order(self) -> None:
        value = {"\ue000": 1.0, "😀": 1e-7, "text": "\b\n\u0000/é"}
        self.assertEqual(
            sa._canonical_json(value),
            '{"text":"\\b\\n\\u0000/é","😀":1e-7,"\ue000":1}',
        )
        self.assertEqual(sa._canonical_json([1e-6, 1e20, -0.0]),
                         '[0.000001,100000000000000000000,0]')
        self.assertEqual(
            sa._canonical_json([333333333.33333329, 1e30, 4.50, 2e-3, 1e-27]),
            '[333333333.3333333,1e+30,4.5,0.002,1e-27]',
        )

    def test_rfc8785_rejects_nonfinite_lossy_numbers_and_lone_surrogates(self) -> None:
        for value in [float("nan"), float("inf"), float("-inf"), "\ud800",
                      9007199254740993]:
            with self.assertRaises(ValueError, msg=repr(value)):
                sa._canonical_json(value)


# ---------------------------------------------------------------------------
# Typed dependency edges and payloads
# ---------------------------------------------------------------------------


class EdgeTests(unittest.TestCase):
    def _graph(self, deps):
        owner = _owner("PR 1", dependencies=tuple(deps))
        parent = _owner("PR 2")
        return sa.reconcile([owner, parent])

    def test_requires_success_needs_no_payload(self) -> None:
        result = self._graph([sa.Dependency("PR 2", "requires_success")])
        self.assertEqual(result.errors, [])
        self.assertEqual(result.records["PR 1"].dependencies[0].kind, "requires_success")

    def test_requires_success_rejects_a_payload(self) -> None:
        result = self._graph([
            sa.Dependency("PR 2", "requires_success", (("artifact", "x"),))])
        self.assertEqual([e.code for e in result.errors],
                         ["invalid_edge_type_or_payload"])

    def test_payload_bearing_kinds_require_their_field(self) -> None:
        for kind, field in [
            ("requires_artifact", "artifact"),
            ("requires_acceptance", "acceptance"),
            ("requires_decision", "decision"),
            ("requires_plan_outcome", "plan_outcome"),
            ("requires_terminal", "terminal_set"),
            ("not_before", "timestamp"),
        ]:
            missing = self._graph([sa.Dependency("PR 2", kind)])
            self.assertEqual([e.code for e in missing.errors],
                             ["invalid_edge_type_or_payload"], kind)
            present = self._graph([sa.Dependency("PR 2", kind, ((field, "v"),))])
            self.assertEqual(present.errors, [], kind)

    def test_unknown_edge_kind_is_rejected(self) -> None:
        result = self._graph([sa.Dependency("PR 2", "requires_magic")])
        self.assertEqual([e.code for e in result.errors],
                         ["invalid_edge_type_or_payload"])

    def test_self_edge_is_rejected(self) -> None:
        result = self._graph([sa.Dependency("PR 1", "requires_success")])
        self.assertEqual([e.code for e in result.errors],
                         ["invalid_edge_type_or_payload"])
        self.assertEqual(result.errors[0].violated_rule, "a slice may not depend on itself")

    def test_unresolved_endpoint_is_rejected(self) -> None:
        result = self._graph([sa.Dependency("PR 99", "requires_success")])
        self.assertEqual([e.code for e in result.errors], ["unresolved_dependency"])

    def test_duplicate_edges_are_deduplicated(self) -> None:
        result = self._graph([
            sa.Dependency("PR 2", "requires_success"),
            sa.Dependency("PR 2", "requires_success"),
        ])
        self.assertEqual(result.errors, [])
        self.assertEqual(len(result.records["PR 1"].dependencies), 1)

    def test_semantically_identical_edges_union_and_sort_source_anchors(self) -> None:
        owner = _owner("PR 1", dependencies=(
            sa.Dependency("PR 2", "requires_artifact", (("artifact", "report"),), "z"),
            sa.Dependency("PR 2", "requires_artifact", (("artifact", "report"),), "a"),
            sa.Dependency("PR 2", "requires_artifact", (("artifact", "report"),), "z"),
        ))
        record = sa.reconcile([owner, _owner("PR 2")]).records["PR 1"]
        self.assertEqual(len(record.dependencies), 1)
        self.assertEqual(record.dependencies[0].all_source_anchors(), ("a", "z"))
        self.assertEqual(record.reconciled_body()["dependencies"][0]["source_anchors"],
                         ["a", "z"])

    def test_authority_keys_resolve_edge_endpoints(self) -> None:
        # An endpoint absent from records but present in the authority key set is known.
        owner = _owner("PR 1", dependencies=(sa.Dependency("PR 2", "requires_success"),))
        result = sa.reconcile([owner], authority_keys=frozenset({"PR 1", "PR 2"}))
        unresolved = [e for e in result.errors if e.code == "unresolved_dependency"]
        self.assertEqual(unresolved, [])


# ---------------------------------------------------------------------------
# Whole-graph cycle detection over gating edges
# ---------------------------------------------------------------------------


class CycleTests(unittest.TestCase):
    def test_two_node_cycle_is_detected(self) -> None:
        a = _owner("PR 1", dependencies=(sa.Dependency("PR 2", "requires_success"),))
        b = _owner("PR 2", dependencies=(sa.Dependency("PR 1", "requires_success"),))
        result = sa.reconcile([a, b])
        self.assertEqual([e.code for e in result.errors], ["cycle"])

    def test_three_node_cycle_is_detected_once(self) -> None:
        a = _owner("PR 1", dependencies=(sa.Dependency("PR 2", "requires_success"),))
        b = _owner("PR 2", dependencies=(sa.Dependency("PR 3", "requires_success"),))
        c = _owner("PR 3", dependencies=(sa.Dependency("PR 1", "requires_success"),))
        result = sa.reconcile([a, b, c])
        self.assertEqual([e.code for e in result.errors], ["cycle"])

    def test_dag_has_no_cycle(self) -> None:
        a = _owner("PR 1", dependencies=(sa.Dependency("PR 2", "requires_success"),))
        b = _owner("PR 2", dependencies=(sa.Dependency("PR 3", "requires_success"),))
        c = _owner("PR 3")
        self.assertEqual(sa.reconcile([a, b, c]).errors, [])

    def test_not_before_edge_does_not_form_a_cycle(self) -> None:
        # not_before is temporal, not gating: it never participates in acyclicity.
        a = _owner("PR 1", dependencies=(
            sa.Dependency("PR 2", "not_before", (("timestamp", "2026-01-01"),)),))
        b = _owner("PR 2", dependencies=(sa.Dependency("PR 1", "requires_success"),))
        self.assertEqual(sa.reconcile([a, b]).errors, [])


# ---------------------------------------------------------------------------
# Explicit-authority join (§2.1 step 3)
# ---------------------------------------------------------------------------


class AuthorityJoinTests(unittest.TestCase):
    def test_declaration_without_authority_key_is_missing_id(self) -> None:
        result = sa.reconcile([_owner("PR 1")], authority_keys=frozenset({"PR 2"}))
        codes = {(e.code, e.normalized_id) for e in result.errors}
        self.assertIn(("missing_id", "PR 1"), codes)  # declaration lacks a key
        self.assertIn(("missing_id", "PR 2"), codes)  # key lacks a declaration

    def test_matching_declaration_and_key_pass(self) -> None:
        result = sa.reconcile([_owner("PR 1")], authority_keys=frozenset({"PR 1"}))
        self.assertEqual([e for e in result.errors if e.code == "missing_id"], [])

    def test_none_authority_skips_the_join(self) -> None:
        result = sa.reconcile([_owner("PR 1")], authority_keys=None)
        self.assertEqual(result.errors, [])


# ---------------------------------------------------------------------------
# Diagnostics ordering and deduplication (§2.1 diagnostic contract)
# ---------------------------------------------------------------------------


class DiagnosticTests(unittest.TestCase):
    def test_byte_identical_diagnostics_are_deduplicated(self) -> None:
        anchor = A("p", 1, 2, "h")
        one = sa._error("malformed_id", anchor, "raw", "rule", "PR 1")
        two = sa._error("malformed_id", anchor, "raw", "rule", "PR 1")
        self.assertEqual(len(sa.sort_diagnostics([one, two])), 1)

    def test_diagnostics_sort_by_the_total_key(self) -> None:
        anchor = A("p", 1, 2, "h")
        later = sa._error("malformed_id", anchor, "z", "rule")
        earlier = sa._error("cycle", anchor, "a", "rule")
        ordered = sa.sort_diagnostics([later, earlier])
        self.assertEqual([d.code for d in ordered], ["cycle", "malformed_id"])

    def test_errors_and_warnings_are_separate_sorted_arrays(self) -> None:
        result = sa.reconcile([
            _owner("PR 4E", acceptance=(
                sa.Criterion("PR-4E-AC-001", "Only.", ("owner",)),)),
            sa.Section("PR 4E", "companion", A("p", 9, 10, "m"),
                       acceptance=(sa.Criterion("PR-4E-AC-002", "Extra.", ("c",)),)),
        ])
        self.assertTrue(all(d.severity == "error" for d in result.errors))
        self.assertTrue(all(d.severity == "warning" for d in result.warnings))


class SourceAnchorTests(unittest.TestCase):
    def test_valid_anchor_and_expected_hash_pass(self) -> None:
        anchor = A("docs/plans/p.md", 1, 2, "block")
        self.assertEqual(sa.validate_source_anchor(anchor, anchor.block_sha256), [])

    def test_bad_bounds_hash_and_expected_hash_are_precise_mismatches(self) -> None:
        anchor = sa.Anchor("", 0, -1, "NOT-A-SHA")
        diagnostics = sa.validate_source_anchor(anchor, "0" * 64)
        self.assertEqual({d.code for d in diagnostics}, {"source_anchor_mismatch"})
        self.assertEqual(len(diagnostics), 5)

    def test_reconcile_reports_invalid_source_anchor(self) -> None:
        section = sa.Section("PR 1", "owner", sa.Anchor("p", 0, 1, "bad"),
                             phase=0, commit_subject="feat: x")
        result = sa.reconcile([section])
        self.assertTrue(any(error.code == "source_anchor_mismatch"
                            for error in result.errors))

    def test_reconcile_verifies_pinned_git_blocks_and_indexed_owner_paths(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.email", "test@example.invalid"],
                           cwd=root, check=True)
            subprocess.run(["git", "config", "user.name", "Test"], cwd=root, check=True)
            plan = root / "docs" / "plans" / "indexed.md"
            plan.parent.mkdir(parents=True)
            plan.write_text("one\ntwo\nthree\n", encoding="utf-8")
            outside = plan.with_name("outside.md")
            outside.write_text("outside\n", encoding="utf-8")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "test: fixture"], cwd=root, check=True)
            commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root,
                                             text=True).strip()
            good = sa.Section("PR 1", "owner", sa.Anchor(
                "docs/plans/indexed.md", 2, 3, _sha("two\nthree")),
                phase=0, commit_subject="feat: x")
            result = sa.reconcile([good], repo_root=root, source_commit=commit,
                                  indexed_plan_paths=frozenset({"docs/plans/indexed.md"}))
            self.assertEqual(result.errors, [])

            stale = sa.Section("PR 1", "owner", sa.Anchor(
                "docs/plans/indexed.md", 2, 3, "0" * 64), phase=0, commit_subject="feat: x")
            outside_owner = sa.Section("PR 2", "owner", sa.Anchor(
                "docs/plans/outside.md", 1, 1, _sha("outside\n")),
                phase=0, commit_subject="feat: x")
            errors = sa.reconcile(
                [stale, outside_owner], repo_root=root, source_commit=commit,
                indexed_plan_paths=frozenset({"docs/plans/indexed.md"}),
            ).errors
            self.assertEqual({error.code for error in errors}, {"source_anchor_mismatch"})
            self.assertTrue(any("pinned Git source block" in error.violated_rule
                                for error in errors))
            self.assertTrue(any("indexed plan set" in error.violated_rule for error in errors))

    def test_real_inventory_record_hash_matches_pinned_commit(self) -> None:
        import plan_inventory

        root = Path(__file__).resolve().parents[4]
        commit = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
        record = next(
            record
            for path in plan_inventory.plan_files(root)
            for record in plan_inventory.scan(path, root)
        )
        section = sa.Section(
            record["ids"][0], "owner",
            sa.Anchor(record["path"], record["line"], record["end_line"],
                      record["block_sha256"]),
            heading=record["heading"], phase=0, commit_subject="docs: fixture",
        )
        result = sa.reconcile(
            [section], repo_root=root, source_commit=commit,
            indexed_plan_paths=frozenset({record["path"]}),
        )
        self.assertEqual(result.errors, [])

    def test_pin_context_is_all_or_none_and_commit_must_be_immutable(self) -> None:
        section = _owner("PR 1")
        root = Path(__file__).resolve().parents[4]
        partials = [
            {"repo_root": root}, {"source_commit": "0" * 40},
            {"indexed_plan_paths": frozenset({section.anchor.path})},
            {"repo_root": root, "source_commit": "0" * 40},
        ]
        for kwargs in partials:
            result = sa.reconcile([section], **kwargs)
            self.assertTrue(any("all-or-none" in error.violated_rule
                                for error in result.errors), kwargs)
        result = sa.reconcile(
            [section], repo_root=root, source_commit="HEAD",
            indexed_plan_paths=frozenset({section.anchor.path}),
        )
        self.assertTrue(any("immutable commit OID" in error.violated_rule
                            for error in result.errors))

    def test_pinned_git_blobs_are_fetched_once_per_unique_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.email", "test@example.invalid"],
                           cwd=root, check=True)
            subprocess.run(["git", "config", "user.name", "Test"], cwd=root, check=True)
            (root / "plan.md").write_text("one\ntwo\n", encoding="utf-8")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "test: fixture"], cwd=root, check=True)
            commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root,
                                             text=True).strip()
            sections = [
                sa.Section("PR 1", "owner", sa.Anchor("plan.md", 1, 1, _sha("one")),
                           phase=0, commit_subject="feat: one"),
                sa.Section("PR 2", "owner", sa.Anchor("plan.md", 2, 2, _sha("two")),
                           phase=0, commit_subject="feat: two"),
            ]
            real_run = subprocess.run
            calls: list[list[str]] = []

            def counted_run(command, *args, **kwargs):
                calls.append(command)
                return real_run(command, *args, **kwargs)

            with mock.patch.object(sa.subprocess, "run", side_effect=counted_run):
                result = sa.reconcile(
                    sections, repo_root=root, source_commit=commit,
                    indexed_plan_paths=frozenset({"plan.md"}),
                )
            self.assertEqual(result.errors, [])
            self.assertEqual(sum(command[:2] == ["git", "show"] for command in calls), 1)


# ---------------------------------------------------------------------------
# Bootstrap manifest locator (§2.1 precedence and typed failures)
# ---------------------------------------------------------------------------


class BootstrapLocatorTests(unittest.TestCase):
    def _repo(self, stack):
        directory = tempfile.TemporaryDirectory()
        stack.append(directory)
        root = Path(directory.name) / "repo"
        (root / ".tracedecay").mkdir(parents=True)
        return root

    def setUp(self) -> None:
        self._stack: list = []

    def tearDown(self) -> None:
        for directory in reversed(self._stack):
            directory.cleanup()

    def _default_manifest(self, root: Path) -> Path:
        path = root / ".tracedecay" / "v2-execution-manifest.json"
        path.write_text('{"schema":"tracedecay.v2.slice-dag/v1","slices":{}}')
        return path

    def test_default_location_is_used_when_present(self) -> None:
        root = self._repo(self._stack)
        expected = self._default_manifest(root)
        found, failure = sa.locate_bootstrap_manifest(root)
        self.assertIsNone(failure)
        self.assertEqual(found, expected.resolve())

    def test_explicit_argument_wins_over_env_and_default(self) -> None:
        root = self._repo(self._stack)
        self._default_manifest(root)
        explicit = Path(self._stack[-1].name) / "explicit.json"
        explicit.write_text('{"schema":"tracedecay.v2.slice-dag/v1","slices":{}}')
        env = Path(self._stack[-1].name) / "env.json"
        env.write_text('{"schema":"tracedecay.v2.slice-dag/v1","slices":{}}')
        found, failure = sa.locate_bootstrap_manifest(root, explicit=str(explicit),
                                                       env=str(env))
        self.assertIsNone(failure)
        self.assertEqual(found, explicit.resolve())

    def test_env_wins_over_default(self) -> None:
        root = self._repo(self._stack)
        self._default_manifest(root)
        env = Path(self._stack[-1].name) / "env.json"
        env.write_text('{"schema":"tracedecay.v2.slice-dag/v1","slices":{}}')
        found, _ = sa.locate_bootstrap_manifest(root, env=str(env))
        self.assertEqual(found, env.resolve())

    def test_missing_candidate_is_typed_failure(self) -> None:
        root = self._repo(self._stack)
        _, failure = sa.locate_bootstrap_manifest(root)
        self.assertEqual(failure.reason, "missing")

    def test_multiple_explicit_values_are_rejected(self) -> None:
        root = self._repo(self._stack)
        _, failure = sa.locate_bootstrap_manifest(root, explicit=["a.json", "b.json"])
        self.assertEqual(failure.reason, "multiple_explicit")

    def test_unknown_repo_identity_is_rejected(self) -> None:
        root = self._repo(self._stack)
        _, failure = sa.locate_bootstrap_manifest(root, repo_identity_ok=False)
        self.assertEqual(failure.reason, "unknown_repo")

    def test_invalid_json_and_wrong_schema_are_typed_failures(self) -> None:
        root = self._repo(self._stack)
        manifest = root / ".tracedecay" / "v2-execution-manifest.json"
        manifest.write_text("not json")
        _, failure = sa.locate_bootstrap_manifest(root)
        self.assertEqual(failure.reason, "invalid_json")
        manifest.write_text('{"schema":"wrong","slices":{}}')
        _, failure = sa.locate_bootstrap_manifest(root)
        self.assertEqual(failure.reason, "schema_mismatch")

    def test_directory_default_is_not_regular(self) -> None:
        root = self._repo(self._stack)
        (root / ".tracedecay" / "v2-execution-manifest.json").mkdir()
        _, failure = sa.locate_bootstrap_manifest(root)
        self.assertEqual(failure.reason, "not_regular")

    def test_default_symlink_escaping_root_is_outside_root(self) -> None:
        root = self._repo(self._stack)
        outside = Path(self._stack[-1].name) / "outside.json"
        outside.write_text('{"schema":"tracedecay.v2.slice-dag/v1","slices":{}}')
        (root / ".tracedecay" / "v2-execution-manifest.json").symlink_to(outside)
        _, failure = sa.locate_bootstrap_manifest(root)
        self.assertEqual(failure.reason, "outside_root")

    def test_explicit_path_outside_root_is_allowed(self) -> None:
        root = self._repo(self._stack)
        outside = Path(self._stack[-1].name) / "outside.json"
        outside.write_text('{"schema":"tracedecay.v2.slice-dag/v1","slices":{}}')
        found, failure = sa.locate_bootstrap_manifest(root, explicit=str(outside))
        self.assertIsNone(failure)
        self.assertEqual(found, outside.resolve())

    @unittest.skipIf(hasattr(os, "geteuid") and os.geteuid() == 0,
                     "root bypasses file permission bits")
    def test_unreadable_default_is_typed_failure(self) -> None:
        root = self._repo(self._stack)
        manifest = self._default_manifest(root)
        os.chmod(manifest, 0)
        try:
            _, failure = sa.locate_bootstrap_manifest(root)
        finally:
            os.chmod(manifest, 0o644)
        self.assertEqual(failure.reason, "unreadable")


# ---------------------------------------------------------------------------
# Pre/post-cutover reconciliation gate (§2.1 step 6)
# ---------------------------------------------------------------------------


class ReconcileAgainstAuthorityTests(unittest.TestCase):
    def _records(self):
        owner = _owner("PR 1", dependencies=(sa.Dependency("PR 2", "requires_success"),))
        parent = _owner("PR 2")
        return sa.reconcile([owner, parent]).records

    def test_matching_candidate_produces_no_diagnostics(self) -> None:
        records = self._records()
        authority = {"slices": {
            nid: {"owner": {
                      "path": rec.owner.path,
                      "heading": rec.owner_heading,
                      "anchor": {"start_line": rec.owner.start_line,
                                 "end_line": rec.owner.end_line,
                                 "block_sha256": rec.owner.block_sha256},
                  },
                  "content_digest": rec.content_digest,
                  "dependencies": [{"parent": dep.parent, "kind": dep.kind,
                                    "payload": dict(dep.payload),
                                    "source_anchors": list(dep.all_source_anchors())}
                                   for dep in rec.dependencies]}
            for nid, rec in records.items()}}
        self.assertEqual(sa.reconcile_against_authority(records, authority, "pre"), [])

    def test_extra_and_missing_ids_are_reconciliation_mismatches(self) -> None:
        records = self._records()
        authority = {"slices": {"PR 2": {}, "PR 9": {}}}  # PR 1 extra, PR 9 missing
        diagnostics = sa.reconcile_against_authority(records, authority, "pre")
        pairs = {(d.code, d.normalized_id) for d in diagnostics}
        self.assertIn(("reconciliation_mismatch", "PR 1"), pairs)
        self.assertIn(("reconciliation_mismatch", "PR 9"), pairs)

    def test_digest_drift_is_a_mismatch(self) -> None:
        records = self._records()
        authority = {"slices": {
            "PR 1": {"content_digest": "sha256:" + "0" * 64,
                     "dependencies": [{"parent": "PR 2", "kind": "requires_success"}]},
            "PR 2": {"content_digest": records["PR 2"].content_digest,
                     "dependencies": []}}}
        diagnostics = sa.reconcile_against_authority(records, authority, "post")
        self.assertTrue(any(d.normalized_id == "PR 1"
                            and "content digest" in d.violated_rule
                            for d in diagnostics))

    def test_edge_drift_is_a_mismatch(self) -> None:
        records = self._records()
        authority = {"slices": {
            "PR 1": {"content_digest": records["PR 1"].content_digest,
                     "dependencies": []},  # candidate has one edge, authority has none
            "PR 2": {"content_digest": records["PR 2"].content_digest,
                     "dependencies": []}}}
        diagnostics = sa.reconcile_against_authority(records, authority, "pre")
        self.assertTrue(any("dependency edges differ" in d.violated_rule
                            for d in diagnostics))

    def test_missing_digest_and_owner_drift_are_mismatches(self) -> None:
        records = self._records()
        authority = {"slices": {
            nid: {"owner": ({**rec.owner.as_dict(), "path": "wrong.md"}
                            if nid == "PR 1" else rec.owner.as_dict()),
                  "dependencies": [{"parent": dep.parent, "kind": dep.kind,
                                    "payload": dict(dep.payload),
                                    "source_anchors": list(dep.all_source_anchors())}
                                   for dep in rec.dependencies]}
            for nid, rec in records.items()}}
        diagnostics = sa.reconcile_against_authority(records, authority, "pre")
        rules = {diagnostic.violated_rule for diagnostic in diagnostics}
        self.assertTrue(any("requires content_digest" in rule for rule in rules))
        self.assertTrue(any("owner differs" in rule for rule in rules))

    def test_typed_payload_and_edge_source_anchor_drift_are_mismatches(self) -> None:
        owner = _owner("PR 1", dependencies=(sa.Dependency(
            "PR 2", "requires_artifact", (("artifact", "report"),), "owner#edge-1"),))
        records = sa.reconcile([owner, _owner("PR 2")]).records
        authority = {"slices": {
            nid: {"owner": rec.owner.as_dict(), "content_digest": rec.content_digest,
                  "dependencies": [
                      {"parent": dep.parent, "kind": dep.kind,
                       "payload": {"artifact": "different"},
                       "source_anchors": ["owner#edge-2"]}
                      for dep in rec.dependencies]}
            for nid, rec in records.items()}}
        diagnostics = sa.reconcile_against_authority(records, authority, "post")
        self.assertTrue(any("dependency edges differ" in d.violated_rule
                            for d in diagnostics))

    def test_compact_owner_and_every_malformed_dependency_are_rejected(self) -> None:
        records = self._records()
        first = records["PR 1"]
        second = records["PR 2"]
        malformed_edges = [
            None, {}, {"parent": "PR 2"},
            {"parent": 2, "kind": "requires_success", "payload": {},
             "source_anchors": []},
            {"parent": "PR 2", "kind": "requires_success", "payload": [],
             "source_anchors": []},
            {"parent": "PR 2", "kind": "requires_success", "payload": {},
             "source_anchors": [3]},
            {"parent": "PR 2", "kind": "requires_success", "payload": {},
             "source_anchor": "legacy"},
        ]
        for malformed in malformed_edges:
            authority = {"slices": {
                "PR 1": {"owner": first.owner.as_dict(),
                         "content_digest": first.content_digest,
                         "dependencies": [malformed]},
                "PR 2": {"owner": second.reconciled_body()["owner"],
                         "content_digest": second.content_digest,
                         "dependencies": []},
            }}
            diagnostics = sa.reconcile_against_authority(records, authority, "pre")
            self.assertTrue(any("malformed dependency" in d.violated_rule
                                for d in diagnostics), malformed)
            self.assertTrue(any("owner differs" in d.violated_rule
                                for d in diagnostics), malformed)

    def test_reconciled_body_and_manifest_projection_are_golden(self) -> None:
        companion = sa.Section("PR 1", "companion", A("companion.md", 4, 5, "c"))
        owner = _owner("PR 1", dependencies=(
            sa.Dependency("PR 2", "requires_success", source_anchor="owner#edge"),))
        sections = [owner, companion, _owner("PR 2")]
        records = sa.reconcile(sections).records
        body = records["PR 1"].reconciled_body()
        self.assertEqual(body["owner"], {
            "path": owner.anchor.path, "heading": "",
            "anchor": {"start_line": owner.anchor.start_line,
                       "end_line": owner.anchor.end_line,
                       "block_sha256": owner.anchor.block_sha256},
        })
        self.assertEqual(body["companions"], [{
            "path": companion.anchor.path, "role": "companion",
            "anchor": {"start_line": companion.anchor.start_line,
                       "end_line": companion.anchor.end_line,
                       "block_sha256": companion.anchor.block_sha256},
        }])
        authority = {"slices": {
            nid: {**record.reconciled_body(), "content_digest": record.content_digest}
            for nid, record in records.items()
        }}
        self.assertEqual(sa.reconcile_against_authority(records, authority, "pre"), [])


# ---------------------------------------------------------------------------
# Canonicalization helper (§2.1 step 4)
# ---------------------------------------------------------------------------


class CanonicalizeTests(unittest.TestCase):
    def test_folds_whitespace_normalizes_newlines_and_trims(self) -> None:
        self.assertEqual(sa.canonicalize_text("  a\t b \r\n c  "), "a b\nc")

    def test_equivalent_text_shares_a_criterion_digest(self) -> None:
        self.assertEqual(sa.criterion_digest("a  b"), sa.criterion_digest("a\tb"))
        self.assertNotEqual(sa.criterion_digest("a b"), sa.criterion_digest("a c"))


if __name__ == "__main__":
    unittest.main()
