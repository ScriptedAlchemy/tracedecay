# Loom concept plates

## Purpose

Measured conversation weave with explicit time-window, replay, transcript, and source-coverage boundaries.

Route: `/loom`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns the shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- A concept plate is not a production authority. The shipping workspace derives each state from its named production response, transport, and authorization paths; otherwise it is unavailable. Loom weave, replay, and chain evidence is named in `../INTERACTION-STATES.md` (`WeaveCanvas`, `ThreadPlayback`, `playback`, `LoomPage`, and `ThreadChain`).

## Canonical semantic-state matrix

| Required semantic or interaction state | Current explainer | Entry condition |
|---|---|---|
| Weave overview | [v3-measured-weave.md](v3-measured-weave.md) | Open `/loom` after a thread page and coverage information are available. |
| Zoom/pan/Fit | No current plate | Required semantic coverage; no current plate. |
| Thread selected | No current plate | Required semantic coverage; no current plate. |
| Thread chain | No current plate | Required semantic coverage; no current plate. |
| Loaded-page replay | [v3-measured-weave.md](v3-measured-weave.md) | Open `/loom` after a thread page and coverage information are available. |
| Loading | No current plate | Required semantic coverage; no current plate. |
| Served empty | No current plate | Required semantic coverage; no current plate. |
| Undated partial | No current plate | Required semantic coverage; no current plate. |
| Stale | No current plate | Required semantic coverage; no current plate. |
| Offline | No current plate | Required semantic coverage; no current plate. |
| Store unavailable | No current plate | Required semantic coverage; no current plate. |
| Source partial | No current plate | Required semantic coverage; no current plate. |
| Source unavailable | No current plate | Required semantic coverage; no current plate. |
| Chain unavailable | No current plate | Required semantic coverage; no current plate. |
| Linked-boundary partial | No current plate | Required semantic coverage; no current plate. |

Rows marked “No current plate” are required coverage, not implied by the selected image.

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
