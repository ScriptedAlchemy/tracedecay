#!/usr/bin/env bash
set -euo pipefail

readonly DEFAULT_BASELINE=".github/conventional-commit-baseline.txt"
readonly CONVENTIONAL_SUBJECT_RE='^(build|chore|ci|docs|feat|fix|perf|refactor|revert|style|test)(\([A-Za-z0-9._/-]+\))?!?: [^[:space:]].*$'

usage() {
    echo "usage: $0 <commit-range-or-sha>" >&2
    echo "       $0 --message-file <path>" >&2
    echo "example: $0 origin/master..HEAD" >&2
}

validate_subject() {
    local label="$1"
    local subject="$2"
    local failed=0

    if ! [[ "$subject" =~ $CONVENTIONAL_SUBJECT_RE ]]; then
        echo "::error::Commit $label does not use conventional commit style: $subject" >&2
        failed=1
    fi

    if [ "${#subject}" -gt 72 ]; then
        echo "::error::Commit $label subject exceeds 72 characters (${#subject}): $subject" >&2
        failed=1
    fi

    if [ "$failed" -ne 0 ]; then
        return 1
    fi

    return 0
}

message_subject() {
    local message_file="$1"
    local line

    while IFS= read -r line || [ -n "$line" ]; do
        line="${line%$'\r'}"
        if [[ "$line" =~ ^[[:space:]]*$ || "$line" =~ ^[[:space:]]*# ]]; then
            continue
        fi
        echo "$line"
        return 0
    done < "$message_file"

    return 1
}

is_merge_subject() {
    local subject="$1"

    [[ "$subject" =~ ^Merge[[:space:]] ]]
}

if [ "$#" -eq 2 ] && [ "$1" = "--message-file" ]; then
    if [ ! -r "$2" ]; then
        echo "Commit message file is not readable: $2" >&2
        exit 2
    fi

    if ! subject="$(message_subject "$2")"; then
        echo "::error::Commit message subject is empty" >&2
        exit 1
    fi

    if is_merge_subject "$subject"; then
        echo "Skipping merge commit message: $subject"
        exit 0
    fi

    if ! validate_subject "message" "$subject"; then
        echo "Commit subjects must look like 'fix: handle UTF-16 files' and stay under 72 characters." >&2
        exit 1
    fi

    echo "OK commit message: $subject"
    exit 0
fi

if [ "$#" -ne 1 ]; then
    usage
    exit 2
fi

range="$1"
baseline_file="${CONVENTIONAL_COMMIT_BASELINE:-$DEFAULT_BASELINE}"
baseline_commits=()

if [ -f "$baseline_file" ]; then
    while IFS= read -r line; do
        line="${line%%#*}"
        line="${line//[[:space:]]/}"
        if [ -n "$line" ]; then
            baseline_commits+=("$line")
        fi
    done < "$baseline_file"
fi

is_baselined_commit() {
    local commit="$1"
    local baseline

    for baseline in "${baseline_commits[@]}"; do
        if [[ "$commit" == "$baseline"* ]]; then
            return 0
        fi
    done

    return 1
}

rev_args=()
if [[ "$range" == *".."* ]]; then
    rev_args=("$range")
else
    rev_args=("-n" "1" "$range")
fi

if ! commits_output=$(git rev-list --reverse --no-merges "${rev_args[@]}"); then
    echo "Failed to resolve commit range: $range" >&2
    exit 2
fi

if [ -z "$commits_output" ]; then
    echo "No non-merge commits to validate in $range"
    exit 0
fi

mapfile -t commits <<< "$commits_output"
failed=0

for commit in "${commits[@]}"; do
    short_sha=$(git rev-parse --short=7 "$commit")
    subject=$(git log -1 --format=%s "$commit")

    if is_baselined_commit "$commit"; then
        echo "Skipping grandfathered commit $short_sha: $subject"
        continue
    fi

    if ! validate_subject "$short_sha" "$subject"; then
        failed=1
    else
        echo "OK $short_sha: $subject"
    fi
done

if [ "$failed" -ne 0 ]; then
    echo "Commit subjects must look like 'fix: handle UTF-16 files' and stay under 72 characters." >&2
    exit 1
fi
