# Task 2 report: existing asset documentation

## Scope

Created a same-stem Markdown explainer for each original V2 PNG and one README for each of the fourteen screen folders. No Task 2 PNG was generated, moved, or edited. Later Brain v8-v10 and Loom v5-v7 iterations are intentionally outside this task's 70-asset baseline.

## Lifecycle basis

The pre-Task-1 canonical table is the conservative basis: one current plate per workspace. Brain v5, v6, and v7 plus Loom v4 are retained first-party explorations documented as rejected for known defects; higher version numbers have no lifecycle effect.

## Verification

Executed from the isolated worktree. This literal fail-fast `zsh` check uses the original 70-image baseline (`asset_base`) and the Task 2 sidecar manifest (`task_base`), so later iterations do not enter Task 2 parity or orphan checks. The final diff check covers every Task 2 repair commit after `task_base`.

```zsh
set -euo pipefail
root=mockups/ui-concept-v2
asset_base=4f2352665
task_base=798c311df

png_stems() {
  git ls-tree -r --name-only "$asset_base" -- "$root" | while IFS= read -r asset; do
    case "$asset" in *.png) print -r -- "${asset%.png}";; esac
  done | sort
}

sidecar_stems() {
  git show --format= --name-only "$task_base" | while IFS= read -r asset; do
    case "$asset" in
      "$root"/[0-9][0-9]-*/README.md) ;;
      "$root"/[0-9][0-9]-*/*.md) print -r -- "${asset%.md}";;
    esac
  done | sort
}

# PNG-to-sidecar parity and no orphan Task 2 sidecars.
diff -u <(png_stems) <(sidecar_stems)

# Every original PNG appears exactly once in its folder README ledger.
for screen in "$root"/[0-9][0-9]-*; do
  for png in $(git ls-tree -r --name-only "$asset_base" -- "$screen" | grep -F '.png'); do
    stem=${png##*/}
    stem=${stem%.png}
    test "$(grep -F -c "[$stem.png]($stem.png)" "$screen/README.md")" -eq 1
  done
done

# Each Task 2 sidecar uses the permitted lifecycle vocabulary and all required sections.
for sidecar in $(git show --format= --name-only "$task_base" | grep -F "$root/" | grep -F '.md' | grep -Fv README.md); do
  grep -F '**Lifecycle:** `current`' "$sidecar" >/dev/null ||
    grep -F '**Lifecycle:** `superseded`' "$sidecar" >/dev/null ||
    grep -F '**Lifecycle:** `rejected`' "$sidecar" >/dev/null
  for heading in '## Intent' '## Entry condition' '## Visible state' '## Supported interactions' '## Truth boundary' '## Lifecycle history'; do
    grep -Fx "$heading" "$sidecar" >/dev/null
  done
done

# No Task 2 PNG path or blob changed after the Task 2 base commit.
git diff --quiet "$task_base..HEAD" -- ":(glob)$root/**/*.png"
git diff --check "$task_base..HEAD"

print -r -- "PASS: screens=14 pngs=$(png_stems | wc -l | tr -d ' ') sidecars=$(sidecar_stems | wc -l | tr -d ' '); exact README indexing, lifecycle vocabulary, required sections, and no Task 2 PNG changes."
```

Output summary:

```text
PASS: screens=14 pngs=70 sidecars=70; exact README indexing, lifecycle vocabulary, required sections, and no Task 2 PNG path/blob changes.
```

## Result

Documentation is limited to `mockups/ui-concept-v2/` and this report. The follow-up commit SHA is in the task handoff.
