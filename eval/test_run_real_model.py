import importlib.util
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("run_real_model.py")
SPEC = importlib.util.spec_from_file_location("run_real_model", MODULE_PATH)
run_real_model = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(run_real_model)


class HermesHomeSelectionTest(unittest.TestCase):
    def test_default_profile_uses_unique_temp_hermes_home(self):
        first = run_real_model.resolve_hermes_profile(run_real_model.parse_args([]))
        second = run_real_model.resolve_hermes_profile(run_real_model.parse_args([]))
        self.addCleanup(first.cleanup)
        self.addCleanup(second.cleanup)

        self.assertEqual(first.profile, run_real_model.DEFAULT_PROFILE)
        self.assertEqual(second.profile, run_real_model.DEFAULT_PROFILE)
        self.assertNotEqual(first.profile_dir, second.profile_dir)
        self.assertIn(tempfile.gettempdir(), str(first.profile_dir))
        self.assertNotEqual(
            first.profile_dir,
            Path.home() / ".hermes/profiles" / run_real_model.DEFAULT_PROFILE,
        )

    def test_explicit_profile_keeps_legacy_user_profile_location(self):
        selected = run_real_model.resolve_hermes_profile(
            run_real_model.parse_args(["--profile", "custom-eval"])
        )
        self.addCleanup(selected.cleanup)

        self.assertEqual(selected.profile, "custom-eval")
        self.assertEqual(selected.profile_dir, Path.home() / ".hermes/profiles/custom-eval")

    def test_explicit_hermes_home_wins(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "explicit-hermes"
            selected = run_real_model.resolve_hermes_profile(
                run_real_model.parse_args(["--hermes-home", str(home)])
            )
            self.addCleanup(selected.cleanup)

            self.assertEqual(selected.profile, run_real_model.DEFAULT_PROFILE)
            self.assertEqual(selected.profile_dir, home)


if __name__ == "__main__":
    unittest.main()
