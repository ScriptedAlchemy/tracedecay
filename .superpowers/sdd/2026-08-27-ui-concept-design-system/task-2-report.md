# Task 2 report: existing asset documentation

## Scope

Created a same-stem Markdown explainer for each original V2 PNG and one README for each of the fourteen screen folders. No Task 2 PNG was generated, moved, or edited.

## Lifecycle basis

The pre-Task-1 canonical table is the conservative basis: one current plate per workspace. Brain v5, v6, and v7 plus Loom v4 are retained first-party explorations documented as rejected for known defects; higher version numbers have no lifecycle effect.

## Verification

Executed from the isolated worktree. The following literal fail-fast `zsh` check scans the current tree, includes the initial Task 2 commit in its PNG-immutability range, scopes every PNG reference to the `## Asset ledger` section, and requires exactly one anchored `design_status:` field per sidecar.

```zsh
set -euo pipefail
root=mockups/ui-concept-v2
task_base=798c311df^

png_stems() {
  while IFS= read -r asset; do print -r -- "${asset%.png}"; done \
    < <(find "$root"/[0-9][0-9]-* -maxdepth 1 -type f -name '*.png' -print | sort)
}

sidecar_stems() {
  while IFS= read -r asset; do print -r -- "${asset%.md}"; done \
    < <(find "$root"/[0-9][0-9]-* -maxdepth 1 -type f -name '*.md' ! -name README.md -print | sort)
}

# Full-tree PNG-to-sidecar parity; diff failure proves a missing or orphan sidecar.
diff -u <(png_stems) <(sidecar_stems)

# Every PNG must appear exactly once inside its screen's Asset ledger.
for screen in "$root"/[0-9][0-9]-*; do
  ledger=$(sed -n '/^## Asset ledger$/,/^## Historical decisions$/p' "$screen/README.md")
  for png in "$screen"/*.png; do
    stem=${png##*/}
    stem=${stem%.png}
    count=$(print -r -- "$ledger" | grep -F -c "[$stem.png]($stem.png)" || true)
    if [ "$count" -ne 1 ]; then
      print -r -- "FAIL: $screen/README.md indexes $stem.png $count times in Asset ledger"
      exit 1
    fi
  done
done

# Exactly one anchored lifecycle field, with no other design_status line.
for sidecar in "$root"/[0-9][0-9]-*/*.md; do
  [[ ${sidecar:t} == README.md ]] && continue
  status_lines=$(grep -E -c '^design_status: (current|superseded|rejected)$' "$sidecar" || true)
  all_status_lines=$(grep -F -c 'design_status:' "$sidecar" || true)
  if [ "$status_lines" -ne 1 ] || [ "$all_status_lines" -ne 1 ]; then
    print -r -- "FAIL: $sidecar needs exactly one anchored design_status field"
    exit 1
  fi
  for heading in '## Intent' '## Entry condition' '## Visible state' '## Supported interactions' '## Truth boundary' '## Lifecycle history'; do
    grep -Fx "$heading" "$sidecar" >/dev/null
  done
done

# This range starts before the initial Task 2 commit.
git diff --quiet "$task_base..HEAD" -- ":(glob)$root/**/*.png"
git diff --check "$task_base..HEAD"
print -r -- "PASS: screens=14 pngs=$(png_stems | wc -l | tr -d ' ') sidecars=$(sidecar_stems | wc -l | tr -d ' '); full-tree parity, exact ledger indexing, lifecycle, sections, and PNG immutability verified."
```

## Actual result

The corrected command was run. It fails fast at full-tree ledger indexing:

```text
pngs=76 sidecars=76
parity=0
FAIL: mockups/ui-concept-v2/01-brain/README.md indexes v10-activity-becomes-synapse.png 0 times in Asset ledger
```

Independent preflight counts also found `design_status=0` and `Lifecycle=70`; therefore the anchored `design_status:` invariant would fail after the six missing ledger rows are resolved. PNG immutability passes for `798c311df^..HEAD`.

## Result

This report now records the full-tree invariant and its actual failing state. Per the report-only constraint, it does not alter the later iteration plates, README ledgers, or sidecar metadata needed to make this check pass.
