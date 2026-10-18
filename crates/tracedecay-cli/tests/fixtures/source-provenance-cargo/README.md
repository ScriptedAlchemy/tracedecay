clean

This two-crate workspace tests Cargo's real build-script rerun behavior without
registry dependencies or a vendored source snapshot. The test supplies the
production Git provenance and watched-input modules through compile-time paths
in the local `fixture-provenance` library. Packaged VCS metadata parsing remains
covered by the test suite's real `serde_json` dependency.

The test copies these sources to an isolated directory including its build
script. Keep the lockfile local-only so `--locked --offline` builds need no
operator Cargo cache or registry access.
