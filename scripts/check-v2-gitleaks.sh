#!/usr/bin/env bash
set -euo pipefail

readonly expected_version="8.30.1"
readonly receipt="target/v2-privacy/receipts/gitleaks-8.30.1.json"
readonly config=".gitleaks.toml"
readonly tree_surface="PR2B-CANDIDATE-TREE"
readonly history_surface="PR2B-REVIEWED-GIT-RANGE"

fail() {
  rm -f -- "$receipt"
  printf 'check-v2-gitleaks: %s\n' "$1" >&2
  exit 1
}

[[ $# -eq 0 || $# -eq 2 ]] || fail "usage: $0 [reviewed_base candidate]"
command -v git >/dev/null 2>&1 || fail "git is required"
command -v gitleaks >/dev/null 2>&1 || fail "gitleaks ${expected_version} is required"
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"
[[ -f "$config" ]] || fail "missing ${config}"

actual_version="$(gitleaks version 2>/dev/null)" || fail "cannot read gitleaks version"
[[ "$actual_version" == "$expected_version" ]] || fail \
  "gitleaks version mismatch: expected ${expected_version}, got ${actual_version}"

candidate="$(git rev-parse --verify 'HEAD^{commit}' 2>/dev/null)" || fail "HEAD is not a commit"
reviewed_base=""
scan_root="."
surface_ids="\"${tree_surface}\""

umask 077
mkdir -p -- "$(dirname -- "$receipt")"
rm -f -- "$receipt"
tmpdir="$(mktemp -d target/v2-privacy/.gitleaks.XXXXXX)" || fail "cannot create temporary directory"
trap 'rm -rf -- "$tmpdir"' EXIT HUP INT TERM
touch "$tmpdir/empty-gitleaks-ignore"

if [[ $# -eq 2 ]]; then
  reviewed_base="$(git rev-parse --verify "$1^{commit}" 2>/dev/null)" || fail "reviewed_base is not a commit"
  candidate="$(git rev-parse --verify "$2^{commit}" 2>/dev/null)" || fail "candidate is not a commit"
  git merge-base --is-ancestor "$reviewed_base" "$candidate" 2>/dev/null || \
    fail "reviewed_base is not an ancestor of candidate or history is incomplete"
  git rev-list --objects --missing=print "$reviewed_base..$candidate" \
    >"$tmpdir/range-objects" 2>/dev/null || fail "cannot enumerate reviewed Git range"
  if grep -q '^?' "$tmpdir/range-objects"; then
    fail "reviewed Git range has missing objects"
  fi
  mkdir "$tmpdir/candidate"
  git archive "$candidate" | tar -xf - -C "$tmpdir/candidate" || \
    fail "cannot materialize candidate tree"
  scan_root="$tmpdir/candidate"
  surface_ids="\"${tree_surface}\", \"${history_surface}\""
fi

tree_report="$tmpdir/tree.json"
if ! gitleaks dir "$scan_root" --config "$config" \
    --gitleaks-ignore-path "$tmpdir/empty-gitleaks-ignore" \
    --no-banner --no-color --redact=100 --max-archive-depth 2 \
    --report-format json --report-path "$tree_report" --log-level error; then
  fail "candidate-tree scan failed or found secrets"
fi
[[ -f "$tree_report" ]] || fail "candidate-tree report is missing"

history_report="$tmpdir/history.json"
printf '[]\n' >"$history_report"
if [[ -n "$reviewed_base" ]]; then
  if ! gitleaks git . --log-opts="$reviewed_base..$candidate" --config "$config" \
      --gitleaks-ignore-path "$tmpdir/empty-gitleaks-ignore" \
      --no-banner --no-color --redact=100 \
      --report-format json --report-path "$history_report" --log-level error; then
    fail "reviewed Git-range scan failed or found secrets"
  fi
  [[ -f "$history_report" ]] || fail "reviewed Git-range report is missing"
fi

finding_count="$(awk '
  BEGIN { count = 0 }
  /^[[:space:]]*\[[[:space:]]*\][[:space:]]*$/ { next }
  { count = 1 }
  END { print count }
' "$tree_report" "$history_report")"
[[ "$finding_count" == "0" ]] || fail "scanner reports are not empty"

config_digest="$(sha256sum "$config" | awk '{print $1}')"
artifact_digest="$({
  printf '%s\n' "$expected_version" "$config_digest" "$reviewed_base" "$candidate"
  sha256sum "$tree_report" "$history_report" | awk '{print $1}'
} | sha256sum | awk '{print $1}')"

base_json="null"
if [[ -n "$reviewed_base" ]]; then
  base_json="\"${reviewed_base}\""
fi

receipt_tmp="$tmpdir/receipt.json"
cat >"$receipt_tmp" <<EOF
{
  "schema_version": 1,
  "tool_name": "gitleaks",
  "tool_version": "${expected_version}",
  "config_digest": "sha256:${config_digest}",
  "reviewed_base_commit": ${base_json},
  "candidate_commit": "${candidate}",
  "scanned_surface_ids": [${surface_ids}],
  "coverage_state": "complete",
  "finding_count": 0,
  "artifact_digest": "sha256:${artifact_digest}"
}
EOF
mv -- "$receipt_tmp" "$receipt"
printf 'gitleaks %s: zero findings; receipt=%s\n' "$expected_version" "$receipt"
