"""Owned profile, daemon and command lifetimes for Unix memory evaluations.

Process groups, parent-death binding and socket readiness are delegated to the
repository's maintained portable-process helper, not reimplemented here.
"""

import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

PROCESS_HELPER = Path(__file__).resolve().parents[2] / "scripts/lib/portable_process.py"
STOP_GRACE_SECONDS = 5


def spawn_owned_command(command, *, cwd=None, env=None, stdout=subprocess.PIPE):
    if os.name != "posix":
        raise RuntimeError("the memory evaluation daemon harness requires Unix process controls")
    if not PROCESS_HELPER.is_file():
        raise FileNotFoundError(f"missing process lifecycle helper: {PROCESS_HELPER}")
    return subprocess.Popen(
        [sys.executable, "-S", str(PROCESS_HELPER), "exec-session",
         "--parent-pid", str(os.getpid()), "--", *map(str, command)],
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=stdout,
        stderr=subprocess.STDOUT,
        text=True,
        start_new_session=True,
    )


def stop_owned_process(process):
    """Stop the owned group and reap its leader, including an exited leader's children."""
    stopped = subprocess.run(
        [sys.executable, "-S", str(PROCESS_HELPER), "stop-group",
         "--pid", str(process.pid), "--grace", str(STOP_GRACE_SECONDS)],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=STOP_GRACE_SECONDS + 5,
    )
    if stopped.returncode not in (0, 2):
        raise RuntimeError(f"could not stop owned process group {process.pid}: {stopped.stderr.strip()}")
    process.wait(timeout=STOP_GRACE_SECONDS + 5)
    return stopped.returncode == 2


def run(command, cwd=None, env=None, timeout=None, check=True):
    """Capture a command without abandoning descendants on timeout or interruption."""
    process = spawn_owned_command(command, cwd=cwd, env=env)
    failure = None
    try:
        output, _ = process.communicate(timeout=timeout)
        result = subprocess.CompletedProcess(command, process.returncode, stdout=output)
        if check and result.returncode != 0:
            raise subprocess.CalledProcessError(result.returncode, command, output=output)
        return result
    except BaseException as error:
        failure = error
        raise
    finally:
        try:
            forced = stop_owned_process(process)
            if forced and failure is None:
                raise RuntimeError("command descendants required forced cleanup")
        except BaseException as cleanup_error:
            if failure is None:
                raise
            failure.add_note(f"process cleanup also failed: {cleanup_error}")
        finally:
            if process.stdout is not None:
                process.stdout.close()


@dataclass
class EvalEnvironment:
    root: Path
    home: Path
    data_dir: Path
    global_db: Path
    env: dict[str, str]
    _daemon: subprocess.Popen | None = field(default=None, repr=False)

    def start_daemon(self, binary, log_path, *, ready_timeout=60):
        if self._daemon is not None:
            raise RuntimeError("this evaluation already owns a daemon")
        with Path(log_path).open("w", encoding="utf-8") as output:
            self._daemon = spawn_owned_command(
                [binary, "daemon", "run", "--socket", self.env["TRACEDECAY_DAEMON_SOCKET"]],
                cwd=self.root,
                env=self.env,
                stdout=output,
            )
        ready = subprocess.run(
            [sys.executable, "-S", str(PROCESS_HELPER), "wait-unix-socket",
             "--path", self.env["TRACEDECAY_DAEMON_SOCKET"],
             "--pid", str(self._daemon.pid), "--timeout", str(ready_timeout)],
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=ready_timeout + 5,
        )
        if ready.returncode != 0:
            raise RuntimeError(f"isolated daemon readiness failed: {ready.stderr.strip()}")
        self.require_daemon_alive()

    def require_daemon_alive(self):
        if self._daemon is None:
            raise RuntimeError("the evaluation has not started its own daemon")
        status = self._daemon.poll()
        if status is not None:
            raise RuntimeError(f"the isolated daemon exited with status {status}")

    def cleanup(self):
        forced = False
        if self._daemon is not None:
            forced = stop_owned_process(self._daemon)
            self._daemon = None
        # Do not delete live process state if group shutdown itself failed.
        try:
            shutil.rmtree(self.root)
        except FileNotFoundError:
            pass
        if forced:
            raise RuntimeError("the isolated daemon required forced cleanup")


def create_eval_environment(scenario_id):
    # A short prefix also keeps macOS temporary socket paths below sockaddr_un.
    root = Path(tempfile.mkdtemp(prefix="td-memory-"))
    try:
        home = root / "home"
        data_dir = root / "profile"
        home.mkdir()
        data_dir.mkdir()
        env = {
            key: value for key, value in os.environ.items()
            if not key.startswith("TRACEDECAY_")
            and key not in {"HERMES_HOME", "HERMES_PROFILE"}
        }
        env.update({
            "HOME": str(home),
            "USERPROFILE": str(home),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "XDG_DATA_HOME": str(home / ".local/share"),
            "XDG_STATE_HOME": str(home / ".local/state"),
            "XDG_CACHE_HOME": str(home / ".cache"),
            "XDG_RUNTIME_DIR": str(root / "runtime"),
            "TRACEDECAY_DATA_DIR": str(data_dir),
            "TRACEDECAY_GLOBAL_DB": str(data_dir / "global.db"),
            "TRACEDECAY_DAEMON_SOCKET": str(root / "daemon.sock"),
        })
        Path(env["XDG_RUNTIME_DIR"]).mkdir(mode=0o700)
        return EvalEnvironment(root, home, data_dir, data_dir / "global.db", env)
    except BaseException:
        shutil.rmtree(root)
        raise
