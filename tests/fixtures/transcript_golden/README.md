# Transcript golden fixtures

This directory contains multi-file native transcript snapshots whose parser
contract depends on filenames, companion metadata, or provider storage roots.
Integration tests copy these files into the real provider layout and run the
production discovery/ingestion path.

Expected files describe canonical facts and relations derived from those native
inputs. They are not generic canonical records and must not bypass the provider
parser. Single-record normalization fixtures live under
`tests/fixtures/provider_normalization/`.
