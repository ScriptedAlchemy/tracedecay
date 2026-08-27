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
| Measured time/host weave | [v7-host-weave-overview.md](v7-host-weave-overview.md) | Depicted by this selected still. |
| Loaded LCM page | [v7-host-weave-overview.md](v7-host-weave-overview.md) | Depicted by this selected still. |
| Time-down, hosts-across axes | [v7-host-weave-overview.md](v7-host-weave-overview.md) | Depicted by this selected still. |
| Vertical session strands | [v7-host-weave-overview.md](v7-host-weave-overview.md) | Depicted by this selected still. |
| Strand width by message count | [v7-host-weave-overview.md](v7-host-weave-overview.md) | Depicted by this selected still. |
| Open, ongoing, recorded, and unknown end states | [v7-host-weave-overview.md](v7-host-weave-overview.md) | Depicted by this selected still. |
| Session hover | No current plate | Required interaction/result is not depicted. |
| Selected-session detail | No current plate | Required interaction/result is not depicted. |
| Zoomed time range | No current plate | Required interaction/result is not depicted. |
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
| [v3-measured-weave.png](v3-measured-weave.png) | [v3-measured-weave.md](v3-measured-weave.md) | `superseded` | Former pre-Task-1 canonical; replaced by the vertical host weave and shared shell in `v7-host-weave-overview`. |
| [v4-luminous-measured-weave.png](v4-luminous-measured-weave.png) | [v4-luminous-measured-weave.md](v4-luminous-measured-weave.md) | `rejected` | Known defect: luminous weave omits loaded-page replay and separate source-coverage conditions. |
| [v5-host-weave-overview.png](v5-host-weave-overview.png) | [v5-host-weave-overview.md](v5-host-weave-overview.md) | `rejected` | Rejected because horizontal strands contradict time-down geometry, live-tail copy contradicts loaded-page playback, and host-pair counts are not served crossing identities. |
| [v6-host-weave-overview.png](v6-host-weave-overview.png) | [v6-host-weave-overview.md](v6-host-weave-overview.md) | `superseded` | Clears the v5 blockers, but was replaced by v7 to align the shared shell and use truthful open/ongoing end-state language. |
| [v7-host-weave-overview.png](v7-host-weave-overview.png) | [v7-host-weave-overview.md](v7-host-weave-overview.md) | `current` | Current Loom plate: loaded-page playback, vertical host strands, truthful end states, and the shared desktop shell. |

## Historical decisions

The pre-Task-1 canonical table selected v3; v7 now supersedes it with vertical host strands, loaded-page playback, truthful end states, and the shared Brain/Loom shell. Loom v4 and v5 are rejected for their recorded defects, while v6 is a valid intermediate superseded by v7. Version stems record iteration order only.
