"""Child-process teardown shared by the packaged-binary acceptance checks."""

from __future__ import annotations

import subprocess


def terminate(process: subprocess.Popen[bytes] | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        # SIGKILL cannot be refused; wait without a timeout so a slow reap
        # here can never mask the real failure being propagated.
        process.wait()
