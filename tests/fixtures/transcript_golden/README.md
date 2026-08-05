# Transcript parser inputs

This directory contains multi-file transcript inputs whose parser contract
depends on filenames, companion metadata, or provider storage roots. Each
family manifest states whether its bytes are a recorded native capture or a
generated adapter-behavior sample. Only a recorded native capture confers
normalization acceptance.

Expected files describe canonical facts and relations derived through the
provider parser. They are not generic canonical records and must not bypass
that parser, but parser traversal alone does not establish provider-schema
provenance. Single-record normalization inputs live under
`tests/fixtures/provider_normalization/`.
