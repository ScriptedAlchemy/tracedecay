import copy
import unittest
from fact_evidence import ExactFactStoreError, added_fact, available_fact, listed_facts, next_after, retained_payload, search_hits


def fact():
    return {
        "fact_id": "fact.seed", "content": "measured content", "source_label": "seed",
        "trust_score_millionths": 300000, "telemetry": {"retrieval_count": 2},
    }


class FactEvidenceTests(unittest.TestCase):
    def test_retained_effect_and_evidence_are_unwrapped_without_legacy_fallback(self):
        payload = {"facts": []}
        for outcome in ["evidence", "effect"]:
            self.assertIs(retained_payload({"outcome": {"outcome": outcome, "value": {"payload": payload}}}, "read"), payload)
        for response in [payload, {"outcome": {"outcome": "problem"}}, {"outcome": {"outcome": "evidence", "value": {}}}]:
            with self.assertRaises(ExactFactStoreError):
                retained_payload(response, "read")

    def test_committed_seed_requires_an_available_fact_and_matching_receipt(self):
        record = fact()
        result = {"outcome": "committed", "result": {
            "disposition": "added", "fact": {"kind": "available", "fact": record},
            "commit": {"fact_id": "fact.seed"},
        }}
        self.assertIs(added_fact(result, "seed"), record)
        bad = copy.deepcopy(result)
        bad["result"]["commit"]["fact_id"] = "fact.unrelated"
        with self.assertRaises(ExactFactStoreError):
            added_fact(bad, "seed")
        for outcome in ["secret_rejected", "normalized_duplicate", "unknown"]:
            with self.assertRaises(ExactFactStoreError):
                added_fact({"outcome": outcome}, "seed")

    def test_list_projections_and_search_hits_use_their_actual_contracts(self):
        record = fact()
        self.assertEqual(listed_facts({"facts": [{"kind": "available", "fact": record}]}, "list"), [record])
        self.assertEqual(search_hits({"hits": [{"fact": record}]}, "search"), [record])
        self.assertEqual(listed_facts({"facts": []}, "list"), [])
        self.assertEqual(search_hits({"hits": []}, "search"), [])
        with self.assertRaises(ExactFactStoreError):
            search_hits({"facts": []}, "search")

    def test_redaction_or_missing_measurements_are_errors_not_zeroes(self):
        with self.assertRaises(ExactFactStoreError):
            available_fact({"kind": "redacted"}, "list")
        for key in ["fact_id", "content", "source_label", "trust_score_millionths", "telemetry"]:
            record = fact()
            del record[key]
            with self.assertRaises(ExactFactStoreError, msg=key):
                available_fact({"kind": "available", "fact": record}, "list")
        record = fact()
        record["telemetry"]["retrieval_count"] = False
        with self.assertRaises(ExactFactStoreError):
            search_hits({"hits": [{"fact": record}]}, "search")

    def test_continuations_cannot_be_missing_or_repeat(self):
        seen = set()
        self.assertEqual(next_after({"next_after": "next"}, seen), "next")
        self.assertIsNone(next_after({"next_after": None}, seen))
        for response in [{}, {"next_after": "next"}, {"next_after": ""}, {"next_after": 4}]:
            with self.assertRaises(ExactFactStoreError):
                next_after(response, seen)


if __name__ == "__main__":
    unittest.main()
