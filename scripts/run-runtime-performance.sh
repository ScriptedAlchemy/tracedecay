#!/bin/sh

set -eu

SCRIPT_DIR=$(CDPATH= cd -P -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -P -- "$SCRIPT_DIR/.." && pwd)

if ! command -v python3 >/dev/null 2>&1; then
    echo "run-runtime-performance.sh: Python 3 is required" >&2
    exit 127
fi

if ! python3 -c 'import sys; raise SystemExit(sys.version_info.major != 3)'; then
    echo "run-runtime-performance.sh: python3 did not provide Python 3" >&2
    exit 2
fi

unset TRACEDECAY_HOME TRACEDECAY_PROFILE TRACEDECAY_PROFILE_DIR

exec python3 "$REPO_ROOT/benchmarks/runtime/run.py" "$@"
