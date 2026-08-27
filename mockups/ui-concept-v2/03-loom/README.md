# Loom concept plates

## Purpose

Measured conversation weave with explicit time-window, replay, transcript, and source-coverage boundaries.

Route: `/loom`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- `dashboard/src/workspaces/loom/WeaveCanvas.tsx`, `dashboard/src/workspaces/loom/ThreadPlayback.tsx`, `dashboard/src/workspaces/loom/ThreadChain.tsx`, and `dashboard/src/workspaces/loom/LoomPage.dom.test.tsx` are the named time-window, replay, and chain authority.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Measured time/host weave | [v3-measured-weave.md](v3-measured-weave.md) | Depicted by this selected still. |
| Recorded thread | [v3-measured-weave.md](v3-measured-weave.md) | Depicted by this selected still. |
| Open thread | [v3-measured-weave.md](v3-measured-weave.md) | Depicted by this selected still. |
| Selected thread | [v3-measured-weave.md](v3-measured-weave.md) | Depicted by this selected still. |
| Causal-crossings page 1 of 8 | [v3-measured-weave.md](v3-measured-weave.md) | Depicted by this selected still. |
| Loaded LCM playback metadata | [v3-measured-weave.md](v3-measured-weave.md) | Depicted by this selected still. |
| Transcript/tool chain | [v3-measured-weave.md](v3-measured-weave.md) | Depicted by this selected still. |
| Commits/edited-files/branch-worktree spans | [v3-measured-weave.md](v3-measured-weave.md) | Depicted by this selected still. |
| Source coverage and freshness | [v3-measured-weave.md](v3-measured-weave.md) | Depicted by this selected still. |
| Load-state legend | [v3-measured-weave.md](v3-measured-weave.md) | Depicted by this selected still. |
| Zoom/pan/Fit controls | No current plate | Required interaction/result is not depicted. |
| Playback transport controls | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-weave.png](v1-weave.png) | [v1-weave.md](v1-weave.md) | `superseded` | Earlier Loom lookbook iteration; replaced by canonical `v3-measured-weave`. |
| [v2-hud-pass-dark.png](v2-hud-pass-dark.png) | [v2-hud-pass-dark.md](v2-hud-pass-dark.md) | `superseded` | Earlier Loom lookbook iteration; replaced by canonical `v3-measured-weave`. |
| [v2-hud-pass-light.png](v2-hud-pass-light.png) | [v2-hud-pass-light.md](v2-hud-pass-light.md) | `superseded` | Earlier Loom lookbook iteration; replaced by canonical `v3-measured-weave`. |
| [v3-measured-weave.png](v3-measured-weave.png) | [v3-measured-weave.md](v3-measured-weave.md) | `current` | Pre-Task-1 canonical selection for Loom. |
| [v4-luminous-measured-weave.png](v4-luminous-measured-weave.png) | [v4-luminous-measured-weave.md](v4-luminous-measured-weave.md) | `rejected` | Known defect: luminous weave omits loaded-page replay and separate source-coverage conditions. |

## Historical decisions

The pre-Task-1 canonical table selects v3. Loom v4 is a first-party exploratory plate rejected for its missing loaded-page replay and separate source-coverage boundary.
