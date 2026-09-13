# Provider normalization fixtures

This directory contains single native provider records and their canonical
envelope expectations. Tests must load the native input and invoke the
provider parser/normalizer; constructing a canonical record directly is not
provider evidence.

Multi-file snapshot providers whose parser contract depends on companion files
live under `tests/fixtures/transcript_golden/`. Those fixtures are exercised
through production discovery and ingestion in addition to focused normalization
tests.

An envelope `version` is TraceDecay's canonical-envelope version, not evidence
that the provider wire format is versioned. `UnknownVersion` coverage belongs
here only when a checked-in provider input proves a genuine unsupported native
schema version.
