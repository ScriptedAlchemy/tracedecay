#!/usr/bin/env python3
"""Exercise harness path selection without building or touching a live profile."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest


class HarnessProjectTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="hermetic harness ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.checkout = self.root / "checkout with spaces"
        self.script = self.checkout / "evals" / "hermetic" / "run.sh"
        self.script.parent.mkdir(parents=True)
        shutil.copyfile(Path(__file__).with_name("run.sh"), self.script)
        self.env_dir = self.root / "isolated env"
        self.env_dir.mkdir()
        self.record = self.root / "invocation.json"
        self.executable = self.env_dir / "record-command"
        self.executable.write_text(
            '#!/usr/bin/env python3\n'
            'import json, os, sys\n'
            'from pathlib import Path\n'
            'Path(os.environ["HARNESS_RECORD"]).write_text(json.dumps({\n'
            '    "args": sys.argv[1:], "home": os.environ["HOME"],\n'
            '    "data": os.environ["TRACEDECAY_DATA_DIR"],\n'
            '    "socket": os.environ["TRACEDECAY_DAEMON_SOCKET"]}))\n'
            'sys.exit(int(os.environ.get("HARNESS_EXIT", "0")))\n'
        )
        self.executable.chmod(0o755)
        (self.env_dir / "env.sh").write_text(
            f"export HERMETIC_TRACEDECAY_BIN={shlex.quote(str(self.executable))}\n"
        )

    def run_index(self, *args, exit_code=0):
        return subprocess.run(
            ["bash", str(self.script), "index", "--env-dir", str(self.env_dir), *args],
            cwd=self.root,
            env={**os.environ, "HARNESS_RECORD": str(self.record),
                 "HARNESS_EXIT": str(exit_code)},
            text=True,
            capture_output=True,
            check=False,
        )

    def test_default_uses_harness_checkout_from_another_cwd(self):
        result = self.run_index()
        self.assertEqual(result.returncode, 0, result.stderr)
        invocation = json.loads(self.record.read_text())
        self.assertEqual(invocation["args"], ["init", str(self.checkout)])
        self.assertEqual(invocation["home"], str(self.env_dir / "home"))
        self.assertEqual(invocation["data"], str(self.env_dir / "tracedecay-data"))
        self.assertEqual(invocation["socket"], str(self.env_dir / "tracedecay-data/daemon.sock"))

    def test_explicit_project_overrides_checkout(self):
        project = self.root / "another project"
        project.mkdir()
        result = self.run_index("--project", str(project))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(self.record.read_text())["args"], ["init", str(project)])

    def test_missing_project_does_not_invoke_binary(self):
        result = self.run_index("--project", str(self.root / "missing"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("project dir does not exist", result.stderr)
        self.assertFalse(self.record.exists())

    def test_index_failure_is_not_reported_as_success(self):
        result = self.run_index(exit_code=17)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("indexing failed", result.stderr)
        self.assertEqual(json.loads(self.record.read_text())["args"], ["init", str(self.checkout)])


if __name__ == "__main__":
    unittest.main()
