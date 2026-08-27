# Task 2 report: existing asset documentation

## Scope

Created a same-stem Markdown explainer for each original V2 PNG and one README for each of the fourteen screen folders. No Task 2 PNG was generated, moved, or edited.

## Lifecycle basis

Lifecycle remains an explicit editorial decision rather than a version-number inference. Brain v8 and Loom v5 are rejected because their explainers record unresolved still defects. Brain v9 and Loom v6 clear those blockers but are superseded shell-harmonization intermediates. Brain v10 and Loom v7 are the current paired plates, replacing the former pre-Task-1 Brain v4 and Loom v3 canonicals. Every other screen retains its documented single current plate.

## Verification

Executed from the isolated worktree. The following literal fail-fast `zsh` check scans the current tree, includes the initial Task 2 commit in its PNG-immutability and whitespace ranges, proves PNG/sidecar parity, rejects orphan or duplicate ledger labels and targets, scopes every PNG reference to the `## Asset ledger` section, requires ledger/sidecar lifecycle agreement with exactly one current plate per screen, and requires one frontmatter-anchored `design_status:` field plus the six required sections per sidecar.

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

# Every ledger label and target must match the screen's PNG set exactly.
for screen in "$root"/[0-9][0-9]-*; do
  ledger=$(sed -n '/^## Asset ledger$/,/^## Historical decisions$/p' "$screen/README.md")
  diff -u \
    <(for png in "$screen"/*.png; do print -r -- "${png:t}"; done | sort) \
    <(print -r -- "$ledger" | sed -n 's/^| \[\([^]]*\.png\)\](\([^)]*\.png\)) |.*$/\1/p' | sort)
  diff -u \
    <(for png in "$screen"/*.png; do print -r -- "${png:t}"; done | sort) \
    <(print -r -- "$ledger" | sed -n 's/^| \[\([^]]*\.png\)\](\([^)]*\.png\)) |.*$/\2/p' | sort)

  current_count=$(print -r -- "$ledger" | grep -F -c '| `current` |' || true)
  if [ "$current_count" -ne 1 ]; then
    print -r -- "FAIL: $screen/README.md has $current_count current plates; expected 1"
    exit 1
  fi

  for png in "$screen"/*.png; do
    stem=${png##*/}
    stem=${stem%.png}
    count=$(print -r -- "$ledger" | grep -F -c "[$stem.png]($stem.png)" || true)
    if [ "$count" -ne 1 ]; then
      print -r -- "FAIL: $screen/README.md indexes $stem.png $count times in Asset ledger"
      exit 1
    fi

    row=$(print -r -- "$ledger" | grep -F "[$stem.png]($stem.png)")
    ledger_status=$(print -r -- "$row" | awk -F '|' '{value=$4; gsub(/[` ]/, "", value); print value}')
    sidecar_status=$(awk -F ': ' '$1 == "design_status" {print $2}' "$screen/$stem.md")
    if [ -z "$ledger_status" ] || [ "$ledger_status" != "$sidecar_status" ]; then
      print -r -- "FAIL: lifecycle mismatch for $screen/$stem (ledger=$ledger_status sidecar=$sidecar_status)"
      exit 1
    fi
  done
done

# Exactly one frontmatter-anchored lifecycle field, with no other design_status line.
for sidecar in "$root"/[0-9][0-9]-*/*.md; do
  [[ ${sidecar:t} == README.md ]] && continue
  status_lines=$(grep -E -c '^design_status: (current|superseded|rejected)$' "$sidecar" || true)
  all_status_lines=$(grep -F -c 'design_status:' "$sidecar" || true)
  if [ "$status_lines" -ne 1 ] || [ "$all_status_lines" -ne 1 ]; then
    print -r -- "FAIL: $sidecar needs exactly one anchored design_status field"
    exit 1
  fi
  if [ "$(sed -n '1p' "$sidecar")" != '---' ] || \
     ! sed -n '2p' "$sidecar" | grep -Ex 'design_status: (current|superseded|rejected)' >/dev/null || \
     [ "$(sed -n '3p' "$sidecar")" != '---' ]; then
    print -r -- "FAIL: $sidecar design_status is not the sole YAML frontmatter field"
    exit 1
  fi
  for heading in '## Intent' '## Entry condition' '## Visible state' '## Supported interactions' '## Truth boundary' '## Lifecycle history'; do
    grep -Fx "$heading" "$sidecar" >/dev/null
  done
done

# This range starts before the initial Task 2 commit and includes the working tree.
git diff --quiet "$task_base" -- ":(glob)$root/**/*.png"
git diff --check "$task_base"
print -r -- "PASS: screens=14 pngs=$(png_stems | wc -l | tr -d ' ') sidecars=$(sidecar_stems | wc -l | tr -d ' '); full-tree parity, no orphans, exact ledger indexing, lifecycle exclusivity, sections, PNG immutability, and whitespace verified."
```

## Actual result

The corrected command was run from the isolated worktree:

```text
PASS: screens=14 pngs=76 sidecars=76; full-tree parity, no orphans, exact ledger indexing, lifecycle exclusivity, sections, PNG immutability, and whitespace verified.
```

## Result

All 76 original PNGs have exactly one same-stem sidecar and exactly one same-screen Asset-ledger row. All 76 sidecars have one lifecycle YAML field that agrees with the ledger; each of the fourteen screens has exactly one current plate. Brain v10 and Loom v7 are current, their intermediate and rejected history is explicit, the original PNG range remains byte-unchanged, and the documentation diff is whitespace-clean.
