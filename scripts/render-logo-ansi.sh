#!/usr/bin/env bash
# Regenerate the tracked terminal logo from its PNG artwork.
#
# crates/tracedecay-cli/src/resources/logo.ansi is embedded by the CLI as-is:
# ordinary builds neither depend on `logo-art` nor render anything. Run this
# after changing logo.png, review, and commit the resulting logo.ansi. The
# `cli` example shipped with `logo-art` prints exactly
# `logo_art::image_to_ansi(png, width)`, so the output is byte-identical to
# what the build script used to render.
#
# usage: scripts/render-logo-ansi.sh
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
resources="$root/crates/tracedecay-cli/src/resources"
width=90

tool_root="$(mktemp -d)"
trap 'rm -rf "$tool_root"' EXIT

cargo install --quiet --locked --root "$tool_root" --example cli logo-art@0.2.1
"$tool_root/bin/cli" "$resources/logo.png" "$width" > "$resources/logo.ansi"
echo "wrote $resources/logo.ansi ($(wc -c < "$resources/logo.ansi") bytes)"
