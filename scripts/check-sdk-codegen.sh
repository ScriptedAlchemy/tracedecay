#!/usr/bin/env sh
set -eu

# The Rust operation descriptors are generated into OUT_DIR by
# crates/tracedecay-sdk/build.rs, so only the published TypeScript package is
# checked in and only it can drift from the canonical registry.
repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

"$repository_root/sdks/codegen/generate.sh"
git -C "$repository_root" diff --exit-code -- sdks/typescript/src
