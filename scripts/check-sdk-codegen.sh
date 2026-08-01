#!/usr/bin/env sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

"$repository_root/sdks/codegen/generate.sh"
git -C "$repository_root" diff --exit-code -- \
  crates/tracedecay-sdk/src/bin/generate.rs \
  crates/tracedecay-sdk/src/operations.rs \
  sdks/typescript/src
