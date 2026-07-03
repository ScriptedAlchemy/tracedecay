#!/usr/bin/env bash
# safe-edit-sequence.sh — the read-only recon half of tracedecay:editing-safely,
# run in one shot via the `tracedecay tool` CLI. Given a symbol (or a struct
# field as `Struct::field`), it prints the recon checklist an agent should
# assemble before the first mutating edit: duplicate/shape twins, every call
# site, rename edges, field write sites, constructor gaps, and shallow impact.
#
# Nothing here mutates the working tree — it only reads the graph. Apply edits
# with the anchored primitives (str_replace / multi_str_replace / insert_at /
# replace_symbol / ast_grep_rewrite), then verify with
# tracedecay:fixing-build-and-type-errors and tracedecay:assessing-impact.
#
# Usage:
#   scripts/safe-edit-sequence.sh <symbol>            # e.g. parse_config
#   scripts/safe-edit-sequence.sh <Struct::field>     # e.g. Config::timeout
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <symbol|Struct::field>" >&2
  exit 2
fi
target="$1"
td() { tracedecay tool "$@"; }

echo "== duplicate / shape twins (reuse before writing) =="
td similar --query "$target" || true
td signature_search --query "$target" || true

if [[ "$target" == *"::"* ]]; then
  echo "== field write sites (blast radius of a field change) =="
  td field_sites --field "$target" || true
  echo "== constructor sites (missing-field lists for a newly required field) =="
  td constructors --struct "${target%%::*}" || true
else
  echo "== call sites (every caller must adapt to a signature change) =="
  td callers --symbol "$target" || true
  echo "== rename edges (preview only — nothing renames) =="
  td rename_preview --symbol "$target" || true
fi

echo "== shallow impact (widen only if widely depended on) =="
td impact --symbol "$target" --max-depth 1 || true

echo
echo "Recon complete. Assemble the checklist above, then apply anchored edits"
echo "and verify — see SKILL.md (Apply / Guardrails)."
