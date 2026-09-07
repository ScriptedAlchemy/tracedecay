# Vendored ruleset provenance — gitleaks

TraceDecay's secret detector no longer carries a hand-curated list of provider
token shapes. The provider catalogue below is vendored from the gitleaks
project and read by `crates/tracedecay-runtime-core/src/privacy/rules.rs`.

## Source

| field | value |
| --- | --- |
| upstream project | gitleaks |
| upstream URL | <https://github.com/gitleaks/gitleaks> |
| vendored file | `config/gitleaks.toml` (upstream path) |
| raw URL | <https://raw.githubusercontent.com/gitleaks/gitleaks/master/config/gitleaks.toml> |
| upstream commit last touching that file | `09242ce9c8a60d9b051fc2d166f9e849b88c7ac0` (2025-11-20) |
| upstream release current at retrieval | `v8.30.1` |
| retrieved | 2026-08-08 |
| `git hash-object gitleaks.toml` | `256f64790ea6d954f0041024be2938089ae1e7a7` |
| `git hash-object LICENSE` | `3c270b36c539ca8fbc4cc1017d3cbb7be1e44bbc` |
| rules in the vendored file | 222 (`[[rules]]` blocks); 221 carry a `regex` |
| licence | MIT — see `LICENSE`, Copyright (c) 2019 Zachary Rice |

`gitleaks.toml` and `LICENSE` are byte-for-byte upstream copies. Nothing in this
directory is edited; that is what makes the `git hash-object` values above a
usable integrity check and what makes a refresh a plain overwrite. All TraceDecay
commentary lives in this file and in `rules.rs`.

## Why a vendored data file rather than a dependency

* Gitleaks is a Go program. There is no maintained Rust crate that packages its
  ruleset, so a dependency would mean either an external binary or a
  reimplementation — both rejected: the detector runs inline at ingest with no
  network and no subprocess.
* The rules are data, not code. Vendoring the TOML keeps the engine (bounded
  scan, structure-aware parse-before-scan layer, typed assessments, entropy and
  context scoring) entirely TraceDecay's.
* `include_str!` binds the file at compile time, so a build is reproducible and
  a running process never reads the ruleset off disk or off the network.

## How TraceDecay reads these rules

Faithful to upstream:

* `regex`, `keywords`, `entropy`, `secretGroup`, and rule-level
  `[[rules.allowlists]]` (`regexes`, `stopwords`, `regexTarget`, `condition`)
  are all honoured.
* `keywords` gate whether a rule runs at all, as upstream does. This is a
  precondition, not an optimisation: `sourcegraph-access-token` accepts a bare
  40-character hex string — every git SHA ever written — and is only safe
  because it never runs unless "sourcegraph" or "sgp_" is nearby.
* Secret extraction follows gitleaks: `secretGroup` when set, otherwise the
  first non-empty capture group, otherwise the whole match.
* The entropy threshold is applied to the extracted secret and a finding is
  dropped when its entropy is at or below the rule's threshold.
* `regexTarget` steers an allowlist's *regexes*. Its stopwords always read the
  secret — aimed at the match instead, the keyword that triggered a rule would
  excuse it.

Deliberate deviations, all in the safe direction:

* **Entropy is scored by our kernel.** `entropy` thresholds are compared against
  `detector_kernel::entropy_bits_per_mille`, TraceDecay's exact fixed-point
  Shannon entropy. Same quantity as upstream's `shannonEntropy`, computed with
  integer arithmetic so a finding's reported score is reproducible. When the
  score cannot be represented the finding is kept, never silently dropped.
* **Redaction covers the whole match, not just the secret group.** A sanitizer
  must not leave secret bytes behind, and the surrounding context a rule matched
  (`api_key = "…"`) is itself worth removing. Entropy still scores the secret.
* **Path-scoped rules and path allowlists are dropped at load.** TraceDecay
  scans in-memory records, not a working tree, so there is no path to match. A
  rule whose only selector is `path` is skipped; an allowlist whose `condition`
  is `AND` over a `paths` criterion can never be satisfied and is skipped, which
  means TraceDecay redacts in cases upstream would have excused.
* **`regexTarget = "line"`** is resolved against the line containing the match
  within the scanned text.
* **Rule identity, not rule prose.** Descriptions are not carried into findings;
  TraceDecay findings never quote matched text, and the detector taxonomy stays
  `PrivacyDetectorV1`.

## Refresh procedure

Manual and deliberate — there is no automation, and there should not be: a
ruleset bump changes what leaves the process, so it wants a human and a diff.

1. Fetch the two files afresh, overwriting in place:

   ```sh
   cd crates/tracedecay-runtime-core/src/privacy/rules/vendor/gitleaks
   curl -fsSL -o gitleaks.toml \
     https://raw.githubusercontent.com/gitleaks/gitleaks/master/config/gitleaks.toml
   curl -fsSL -o LICENSE \
     https://raw.githubusercontent.com/gitleaks/gitleaks/master/LICENSE
   ```

2. Read `git diff` on `gitleaks.toml`. Look for: new rules (fine), removed rules
   (check nothing in `../../supplement.toml` was written assuming them), and any
   new TOML key the loader ignores. `rules.rs` rejects a document it cannot parse
   rather than degrading to an empty ruleset, so a schema change fails loudly at
   the first detector call — but a *silently ignored* new key would not, and that
   is the thing to read for.

3. Confirm the licence is still MIT and the copyright line is unchanged.

4. Record the new provenance here: upstream commit for `config/gitleaks.toml`
   (`gh api "repos/gitleaks/gitleaks/commits?path=config/gitleaks.toml&per_page=1"`),
   current release tag, retrieval date, and both `git hash-object` values.

5. Run the privacy suites plus `memory::hygiene`. Every rule is compiled at load,
   so an upstream regex that Rust's `regex` crate cannot accept surfaces as a
   typed `CredentialRuleSetError::Regex` naming the rule id.

   ```sh
   cargo test -p tracedecay-runtime-core --lib privacy
   cargo test -p tracedecay-runtime-core --lib memory::hygiene
   ```

6. Commit the data refresh on its own, separately from any engine change.

Gitleaks regexes are written for Go's RE2, which — like Rust's `regex` crate —
has no backreferences and no lookaround. That is why the catalogue transfers at
all. The two engines disagree in exactly two places, and `rules.rs` translates
both at load rather than editing the vendored file:

* RE2 reads a `{` that opens no valid repetition as a literal; Rust refuses it.
  Upstream depends on the RE2 reading (`^\$(?:\d+|{\d+})$` matches a shell
  placeholder). Those braces are escaped; real quantifiers are untouched.
* RE2's `\w` is exactly `[0-9A-Za-z_]`; Rust's is Unicode-aware. Reading it the
  Rust way is both a different match and far larger to compile — three rules
  that repeat `\w` over a wide bound exceed the 10 MB program limit. Expanding
  `\w` to its RE2 meaning fixes the semantics and the size together, so every
  rule compiles under the default limit with no rule dropped.

`\W`, `\D` and `\S` would need the same treatment and appear nowhere in the
catalogue today. A refresh that introduces one, or any construct Rust has no
equivalent for, fails at step 5 with a typed error naming the rule id.
