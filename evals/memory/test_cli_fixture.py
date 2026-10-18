"""Exercise the selected real CLI's isolated init and fact store without a model."""
import os
import sys
import tempfile
import unittest
from pathlib import Path

import run_real_model as runner


class CliFixtureTest(unittest.TestCase):
    def test_current_cli_initializes_and_seeds_an_owned_profile(self):
        binary = Path(os.environ["TRACEDECAY_MEMORY_EVAL_TEST_BIN"]).resolve(strict=True)
        environment = runner.create_eval_environment("cli-fixture")
        root = environment.root
        fixture = root / "fixture"
        with tempfile.TemporaryDirectory() as logs:
            log = Path(logs) / "daemon.log"
            try:
                environment.start_daemon(str(binary), log)
                scenario = {"id": "cli-fixture", "setup": {"files": {}, "facts": [{
                    "content": "Durable fixture decision: use the isolated daemon for evaluation.",
                    "category": "decision", "source_label": "fixture-seed", "trust": 0.8,
                }]}}
                runner.build_fixture(scenario, str(binary), environment.env, fixture)
                records = runner.list_fact_records(str(binary), fixture, environment.env)
                seeded = [r for r in records if r["source_label"] == "fixture-seed"]
                self.assertEqual(len(seeded), 1)
                self.assertEqual(seeded[0]["content"], scenario["setup"]["facts"][0]["content"])
                self.assertIsInstance(seeded[0]["fact_id"], str)
                environment.require_daemon_alive()
            except BaseException:
                if log.exists():
                    print(log.read_text()[-16000:], file=sys.stderr)
                raise
            finally:
                runner.cleanup_eval_artifacts(fixture, environment)
        self.assertFalse(root.exists())


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("expected the selected real tracedecay binary")
    os.environ["TRACEDECAY_MEMORY_EVAL_TEST_BIN"] = str(Path(sys.argv.pop()).resolve(strict=True))
    unittest.main()
