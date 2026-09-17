#!/usr/bin/env bash
# Canonical RUSTFLAGS for Hotpath lanes that need --cfg tokio_unstable.
#
# Cargo's env RUSTFLAGS *replaces* rustflags from every cargo config
# (repo .cargo/config.toml and ~/.cargo/config.toml) entirely. Exporting
# only `--cfg tokio_unstable` therefore drops the linker and debuginfo
# flags those configs would have supplied, so the heaviest, most-rebuilt
# profiling binaries link without mold and with packed debuginfo.
#
# This file is the single composition point. Source it before cargo:
#
#   source scripts/hotpath-rustflags.sh
#   cargo build --profile perf ... --features production,hotpath
#
# Or eval the printed export when a subshell cannot source:
#
#   eval "$(scripts/hotpath-rustflags.sh)"
#
# These flags are what the workspace considers canonical for a hotpath
# lane on Linux gnu targets — not a live parse of cargo config. Machine
# config can change (split-debuginfo landed in ~/.cargo/config.toml
# overnight); do not invent a parser. Update this export when the
# workspace's intended hotpath flag set changes.
#
# Canonical set:
#   --cfg tokio_unstable          Tokio runtime metrics Hotpath needs
#   -C link-arg=-fuse-ld=mold     machine/workspace linker (env would drop it)
#   -C split-debuginfo=unpacked   cheaper debuginfo on gnu (env would drop it)

export RUSTFLAGS='--cfg tokio_unstable -C link-arg=-fuse-ld=mold -C split-debuginfo=unpacked'

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    printf 'export RUSTFLAGS=%q\n' "$RUSTFLAGS"
fi
