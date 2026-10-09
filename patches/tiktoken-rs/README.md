# tiktoken-rs (TraceDecay path patch)

Path-patched `tiktoken-rs` 0.12.1 that ships only a gzip-compressed
`o200k_base` vocabulary. Legacy `cl100k` / `p50k` / `r50k` assets are omitted;
their public constructors return a typed error so they cannot re-enter the
binary through a silent include.

TraceDecay's production token-counting path uses `o200k_base` exclusively
(see #3272). Gzip-embedding that vocabulary cuts ~1.8 MiB from the stripped
dist CLI versus the upstream plaintext `include_str!`.
