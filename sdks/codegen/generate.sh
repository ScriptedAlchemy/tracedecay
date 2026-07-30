#!/usr/bin/env sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
exec cargo run \
    --manifest-path "$repository_root/sdks/codegen/Cargo.toml" \
    --bin generate \
    -- "$repository_root"
