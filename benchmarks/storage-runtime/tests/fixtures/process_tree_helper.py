from __future__ import annotations

import os
import signal
import subprocess
import sys
import time
from pathlib import Path


def main() -> None:
    role = sys.argv[1]
    pid_directory = Path(sys.argv[2])
    if sys.platform != "win32":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)

    (pid_directory / f"{role}.pid").write_text(str(os.getpid()), encoding="ascii")
    if role == "root":
        subprocess.Popen(
            [sys.executable, __file__, "child", str(pid_directory)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    elif role == "child":
        subprocess.Popen(
            [sys.executable, __file__, "grandchild", str(pid_directory)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

    while True:
        time.sleep(1)


if __name__ == "__main__":
    main()
