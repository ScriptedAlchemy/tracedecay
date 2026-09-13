# Visual audit baselines

This directory holds the **committed reference screenshots** the visual audit
diffs against. It ships empty on purpose — baselines are established
deliberately, not auto-generated, so a diff always compares against a reviewed
image.

## How it works

`npm run visual:audit` renders every registered surface
(`stories/registry.ts`) across light+dark themes at 320 / 768 / 1440 widths and
writes PNGs + `manifest.json` to `../audit-gallery/` (generated,
git-ignored).

`npm run visual:audit -- --diff` re-runs the capture and, for each screenshot,
runs [pixelmatch](https://github.com/mapbox/pixelmatch) against the baseline of
the **same filename** in this directory:

| baseline state        | manifest `diff.status` |
| --------------------- | ---------------------- |
| file present, equal   | `match`                |
| file present, differs | `diff` (+ diff PNG in `audit-gallery/diffs/`) |
| file absent           | `no-baseline`          |
| dimensions differ     | `size-mismatch`        |

## Establishing / updating baselines

1. Run `npm run visual:audit` and review `../audit-gallery/`.
2. When the surfaces look correct, copy the approved PNGs here:
   ```sh
   cp ../audit-gallery/*.png .
   ```
3. Commit the baselines. Subsequent `--diff` runs flag any drift.

Filenames follow `<surface-id>__<theme>__<width>.png`
(e.g. `observatory__dark__1440.png`).
