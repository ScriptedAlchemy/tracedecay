#!/usr/bin/env bash
set -euo pipefail

python3 - .github/workflows/sdk-conformance.yml <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
if '      - "tests/sdk_conformance_non_vacuity_test.sh"' not in workflow:
    raise SystemExit("SDK conformance workflow paths do not cover the mutation test")
marker = "  production-router:"
if marker not in workflow:
    raise SystemExit("SDK conformance workflow is missing production-router")
job = re.split(r"\n  (?=\S)", workflow.split(marker, 1)[1], maxsplit=1)[0]
for required in [
    "REQUIRE_EXACT_TEST_COUNT=nonzero",
    "scripts/require-exact-test.sh",
    "tests/sdk_conformance_non_vacuity_test.sh",
    "--test production_daemon",
    "--ignored",
]:
    if required not in job:
        raise SystemExit(
            f"SDK ignored conformance lane must preserve {required!r}"
        )
PY

fake_ignored_cargo() {
  case " $* " in
    *" --test production_daemon "*" --ignored "*) ;;
    *)
      echo "fake SDK cargo received the wrong selection: $*" >&2
      return 2
      ;;
  esac
  if [[ ${FAKE_IGNORED_SELECTION_EMPTY:-0} == 1 ]]; then
    printf '%s\n' \
      "running 0 tests" \
      "test result: ok. 0 passed; 0 failed; 0 ignored"
  else
    printf '%s\n' \
      "running 1 test" \
      "test result: ok. 1 passed; 0 failed; 0 ignored"
  fi
}
export -f fake_ignored_cargo

sdk_command=(
  bash -c 'fake_ignored_cargo "$@"' _
  cargo test --manifest-path crates/tracedecay-sdk/Cargo.toml
  --test production_daemon -- --ignored
)

REQUIRE_EXACT_TEST_COUNT=nonzero \
  scripts/require-exact-test.sh "${sdk_command[@]}"

set +e
negative_output="$(
  FAKE_IGNORED_SELECTION_EMPTY=1 REQUIRE_EXACT_TEST_COUNT=nonzero \
    scripts/require-exact-test.sh "${sdk_command[@]}" 2>&1
)"
negative_status=$?
set -e
if [[ $negative_status -eq 0 ]]; then
  echo "SDK ignored conformance lane accepted a zero-test selection" >&2
  exit 1
fi
if [[ $negative_output != *"expected 'at least one passed; 0 failed'"* ]]; then
  echo "$negative_output" >&2
  echo "SDK ignored zero-test rejection did not come from the nonzero guard" >&2
  exit 1
fi
