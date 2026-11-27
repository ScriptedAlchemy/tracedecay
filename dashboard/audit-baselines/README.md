# Visual audit baselines

This directory holds the committed reference screenshots that the visual audit
diffs against. It ships empty on purpose. A person establishes baselines after
reviewing them, and nothing generates them automatically, so a diff always
compares against a reviewed image.

## How the audit works

`pnpm run visual:audit` renders every registered surface
(`stories/registry.ts`) in the light and dark themes at widths of 320, 768, and
1440 px. It writes PNGs and `manifest.json` to `../audit-gallery/`, which is
generated and git-ignored.

`pnpm run visual:audit -- --diff` re-runs the capture. For each screenshot it
runs [pixelmatch](https://github.com/mapbox/pixelmatch) against the baseline
with the same filename in this directory:

| baseline state        | manifest `diff.status` |
| --------------------- | ---------------------- |
| file present, equal   | `match`                |
| file present, differs | `diff` (and a diff PNG in `audit-gallery/diffs/`) |
| file absent           | `no-baseline`          |
| dimensions differ     | `size-mismatch`        |

## Establishing and updating baselines

1. Run `pnpm run visual:audit` and review `../audit-gallery/`.
2. When the surfaces look correct, copy the approved PNGs here:
   ```sh
   cp ../audit-gallery/*.png .
   ```
3. Commit the baselines. Later `--diff` runs flag any change.

Filenames follow `<surface-id>__<theme>__<width>.png`
(for example `observatory__dark__1440.png`).
