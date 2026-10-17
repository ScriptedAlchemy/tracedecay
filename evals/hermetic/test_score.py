import unittest
from score import evaluate_scenario

class NegativeExpectationTests(unittest.TestCase):
    def grade(self, calls, status=0):
        return evaluate_scenario({"anti_tools": ["tracedecay_fact_store_add"]}, None, None, calls, [], [], verify_status=status, agent_completed=True)

    def test_verified_abstention_passes(self):
        self.assertTrue(self.grade([])["pass"])

    def test_forbidden_mcp_attempt_fails_even_if_store_verifier_passes(self):
        result = self.grade(["tracedecay_fact_store_add"])
        self.assertFalse(result["pass"])
        self.assertEqual(result["anti_tools_used"], ["tracedecay_fact_store_add"])

    def test_missing_agent_turn_is_not_abstention(self):
        result = evaluate_scenario({"anti_tools": ["tracedecay_fact_store_add"]}, None, None, [], [], [], verify_status=0)
        self.assertFalse(result["pass"])

    def test_native_read_ban_does_not_ban_tracedecay_read(self):
        result = evaluate_scenario({"expected_tools": ["tracedecay_read"], "anti_tools": ["Read"]}, None, None, ["tracedecay_read"], [], [], agent_completed=True)
        self.assertTrue(result["pass"])

    def test_failed_verification_still_fails(self):
        self.assertFalse(self.grade([], 1)["pass"])

if __name__ == "__main__":
    unittest.main()
