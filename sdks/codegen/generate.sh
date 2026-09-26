#!/usr/bin/env sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
# Cargo reads .cargo/config.toml from the working directory, and this
# workspace has its own pnpm-vendored sources and lockfile.
cd -- "$repository_root/sdks/codegen"
exec cargo run --bin generate -- "$repository_root"
