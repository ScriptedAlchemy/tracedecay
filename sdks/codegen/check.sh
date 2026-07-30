#!/usr/bin/env sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
temporary_directory=$(mktemp -d)
trap 'rm -rf "$temporary_directory"' EXIT HUP INT TERM

generated_files='
sdks/typescript/src/index.ts
sdks/typescript/src/operations.ts
sdks/typescript/src/types.ts
'

for relative_path in $generated_files; do
    mkdir -p "$temporary_directory/$(dirname -- "$relative_path")"
    cp "$repository_root/$relative_path" "$temporary_directory/$relative_path"
done

sh "$repository_root/sdks/codegen/generate.sh"

status=0
for relative_path in $generated_files; do
    if ! cmp -s "$temporary_directory/$relative_path" "$repository_root/$relative_path"; then
        diff -u "$temporary_directory/$relative_path" "$repository_root/$relative_path" || true
        status=1
    fi
done

if [ "$status" -ne 0 ]; then
    printf '%s\n' 'generated TypeScript SDK files are stale' >&2
    exit "$status"
fi

printf '%s\n' 'generated TypeScript SDK files are current'
