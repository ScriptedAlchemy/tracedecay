#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=../scripts/lib/gate-test.sh
. "$(dirname "${BASH_SOURCE[0]}")/../scripts/lib/gate-test.sh"

checker="$GATE_REPO_ROOT/scripts/check-dashboard-digests.sh"
a=$(printf 'a%.0s' {1..64})
b=$(printf 'b%.0s' {1..64})

same="$GATE_SCRATCH/same"
mkdir -p "$same"
printf '%s\n' "$a" >"$same/dashboard-sha256-x86_64-linux.txt"
printf '%s\r\n' "$a" >"$same/dashboard-sha256-x86_64-windows.txt"
gate_run bash "$checker" "$same"
gate_expect_success "identical digests"
gate_output_contains "identical digests" "2 release targets embed dashboard $a"

drift="$GATE_SCRATCH/drift"
mkdir -p "$drift"
printf '%s\n' "$a" >"$drift/dashboard-sha256-x86_64-linux.txt"
printf '%s\n' "$b" >"$drift/dashboard-sha256-aarch64-macos.txt"
gate_run bash "$checker" "$drift"
gate_expect_failure "drifted digests"
gate_output_contains "drifted digests" "aarch64-macos.txt: $b"

empty="$GATE_SCRATCH/empty"
mkdir -p "$empty"
: >"$empty/dashboard-sha256-x86_64-linux.txt"
gate_run bash "$checker" "$empty"
gate_expect_failure "empty digest"
gate_output_contains "empty digest" "not a dashboard bundle digest"

gate_run bash "$checker" "$GATE_SCRATCH/none"
gate_expect_success "no rebuilt targets"
gate_output_contains "no rebuilt targets" "0 release targets"
