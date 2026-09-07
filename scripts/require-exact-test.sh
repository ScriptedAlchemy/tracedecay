#!/usr/bin/env bash
# Run one name-filtered cargo test and require that it actually ran.
#
# `cargo test <name> -- --exact` exits 0 when the filter matches NOTHING:
#
#     running 0 tests
#     test result: ok. 0 passed; 0 failed; 0 ignored; 118 filtered out
#
# A rename, a moved #[cfg], or a feature change therefore turns a CI gate into
# a silent no-op that still reports success. This is not hypothetical: the
# Windows durable job carried a nextest clause that matched nothing for months.
# nextest has --no-tests=fail for this; libtest does not, so assert on the count
# libtest reports instead of on the exit code alone.
#
# usage: scripts/require-exact-test.sh cargo test --test foo the_test -- --exact
#
# Set REQUIRE_EXACT_TEST_COUNT to a positive integer, or to `nonzero` for a
# single-target suite whose membership may evolve but must never become empty.
set -uo pipefail

readonly EXPECTED="${REQUIRE_EXACT_TEST_COUNT:-1}"

if [[ $EXPECTED != nonzero && ! $EXPECTED =~ ^[1-9][0-9]*$ ]]; then
    echo "require-exact-test: REQUIRE_EXACT_TEST_COUNT must be a positive integer or nonzero" >&2
    exit 2
fi

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <command...>" >&2
    echo "example: $0 cargo test --test foo the_test -- --exact" >&2
    exit 2
fi

output="$(mktemp)"
# shellcheck disable=SC2064 # expand $output now, not at trap time
trap "rm -f '$output'" EXIT

"$@" 2>&1 | tee "$output"
status="${PIPESTATUS[0]}"
if [ "$status" -ne 0 ]; then
    echo "require-exact-test: command exited $status" >&2
    exit "$status"
fi

summaries="$(grep -c '^test result: ' "$output" || true)"
if [ "$summaries" -ne 1 ]; then
    echo "require-exact-test: expected exactly one libtest summary line," \
        "found $summaries. Scope the command to a single test target so the" \
        "count below is unambiguous." >&2
    exit 1
fi

if [[ $EXPECTED == nonzero ]]; then
    expected_pattern='^test result: ok\. [1-9][0-9]* passed; 0 failed'
    expected_description='at least one passed; 0 failed'
else
    expected_pattern="^test result: ok\\. ${EXPECTED} passed; 0 failed"
    expected_description="${EXPECTED} passed; 0 failed"
fi
if ! grep -Eq "$expected_pattern" "$output"; then
    echo "require-exact-test: expected '$expected_description' but got:" >&2
    grep '^test result: ' "$output" >&2
    echo "The filter matched no test (libtest exits 0 for that), so this gate" \
        "proved nothing. Fix the test name, feature set, or #[cfg]." >&2
    exit 1
fi
