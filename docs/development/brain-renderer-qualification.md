# Brain renderer qualification

Observed 2026-09-09. This is a bounded product-renderer
qualification, not a claim of final plate parity or real workload admission.

## Sources and environment

Product baseline: `e0ace8f10bdb30421163fe0a059307c07b3263ba` on the reopened
V2 integration branch. The inspected implementation adds commits `af3000169`,
`ac86bc311`, `f820b6fb2`, `d9d2ed0e8`, `115c33582`, `afeb8b761`,
`8d8da50cd`, and `740d42018`. The authoritative five Brain plates and same-stem
briefs are in `mockups/ui-concept-v2/01-brain/final/`. The independent reference
for renderer comparison is the demo's `lookbook/IMPLEMENTATION.md`.

Installed product dependencies: React/React DOM 19.2.8, Sigma 3.0.3,
Graphology 0.26.0. No production renderer dependency was added.
Browser: Chrome 152.0.0.0, Linux x86_64, device-pixel ratio 1; ANGLE over Mesa
llvmpipe (LLVM 20.1.2, 256 bits), OpenGL 4.5. Captures use 1440×900, 768×900,
and 320 CSS-pixel layouts. The browser reports visible and focused.

The running product uses its normal generated decoder, React shell, Sigma
renderer, and EventSource connection. An external read-only HTTP fixture serves
`dashboard/stories/fixtures/data.ts`; load variants clone those shapes with
explicit `load-symbol-*` identities. No daemon, profile, production registry,
real session, or delivery admission was mutated. **All captured data is
concept-synthetic, including envelopes displayed under the product's admitted
event heading.** The fixture's transport is not proof that a real daemon admitted
those events. Missing transcript identities remain visibly unavailable.

Local artifacts are under `/fast/projects/td-visual-review/issue1099/`.
`gallery.html` pairs the five runtime states with their plate references;
`api.ts` describes the external fixture transport. These files are review
artifacts, not a second production authority or a shipping raster background.

## Observed behavior and fixes

| State | Runtime evidence | Result and remaining visual difference |
| --- | --- | --- |
| Registry overview | `brain-overview-final.png` | Measured recency/mass coordinates retained. Smooth companion falloff replaces stacked discs. Null companion labels stop decorative nodes claiming Sigma label-grid cells. This remains a measured sparse field rather than the plate's rich particle body. |
| Independent inspection | `brain-inspection-final.png` | Canvas hover and registry keyboard focus resolve the same exact project ID without changing scope or emitting activity. Static inspection prints registry/source evidence. A permanently reserved inspector aperture prevents rows moving between pointer-down and click. |
| Repository zoom | `brain-repository-final.png` | Only actual shared Git identity forms a hub. Repository view retains measured coordinates, with breadcrumb and overview minimap. Unknown Git identity never gains an invented membership edge. |
| Scoped project | `brain-scoped-final.png`, `brain-to-code-fixed.png` | Actual returned symbol IDs/relations have a DOM equivalent. Camera buttons and keyboard controls work. NavRail and CommandPalette preserve selected project when navigating to Code or other workspaces. |
| Admitted activity | `brain-event-inspection-final.png`, `brain-20000-events.png` | Accepted synthetic wire frames retain canonical event ID and server observation time. Selecting a retained event inspects its exact drawn project; unscoped activity cannot silently light the active project. The envelope has no transcript identity, so no fabricated transcript target appears. |

The production 4,200 ms registry heat half-life and one drawn relation hop are
preserved. A checkout event can light its drawn repository hub, not sibling
checkouts. Pointer/focus/selection do not create pulses. Symbol graphs explicitly
say that no symbol activity was supplied.

The browser also exposed an idle resize bug: Sigma resized and cleared its
buffer without scheduling a refresh. The corrected canvas redraws after resize.
A real `WEBGL_lose_context` loss showed the typed unavailable state while retaining
DOM identities; restoring the context redrew the graph. Screenshots:
`brain-context-lost.png` and `brain-context-restored.png`. After settled navigation
to Settings, the Sigma node-canvas count is zero; returning to Brain gives one
(`cleanup.json`). This proves canvas unmount/remount, not an exhaustive GPU leak
measurement.

Forced colors and reduced motion were exercised together at 320 pixels.
`brain-320-forced-colors.png` shows the canvas retaining its contrast aperture;
DOM controls retain native forced-color behavior. The measured page width is
320/320 with no horizontal document overflow. Keyboard zoom, pan, Fit, selection,
and dismissal have DOM routes. Actual 200% browser zoom and injected worker
failure are not qualified by the narrow-viewport capture.

## Load evidence and limits

| Input | Actual observed result |
| --- | --- |
| 5,000 returned symbols, 4,999 chain relations | Generated payload admitted and 5,000-item DOM list present; actual Sigma canvas drawn, keyboard pan and zoom respond. `brain-5000-symbols.png`. The fitted overview is visibly dense and does not make individual symbols readable without zoom or the DOM list. |
| 20,000 returned symbols | No canvas mounted. Truthful 5,000-symbol limit shown, exact returned DOM identities retained. `brain-20000-fallback.png`. No nonexistent alternate renderer is advertised; the admission limit was not raised. |
| 5,000 synthetic SSE envelopes | HTTP fixture transmitted 5,000 frames in 2,507 ms. The normal EventSource path received them; bounded retention remained 64. `sse-overview-5000.json`. |
| 20,000 further synthetic SSE envelopes | Transmitted in 10,064 ms; UI retained exact terminal `run-1099-1700000000000000:dashboard_activity:45000`, and selecting it inspected `tracedecay`. `sse-overview-20000.json`, `brain-20000-events.png`. This proves terminal acceptance and bounded retention, not persistence of every intermediate frame. |

A load-discovered label bug is fixed: the old “per min” number counted only the
64-pulse ring. It now reads “retained · last 60s” and explicitly says counts cover
the retained window rather than the entire stream. No retention budget changed.

The frame sampler uses requestAnimationFrame intervals, not fabricated renderer
frame timings. At 5,000 symbols it recorded 15 intervals over 15,209 ms,
p50 1,016.6 ms, p95 1,016.7 ms. The 29-project overview recorded 16 intervals
over 15,825 ms, with the same p50/p95. An idle repeat also returned ~1,016.6 ms.
These results are **not a production performance pass**: the selected browser's
software-rendering environment exhibits approximately 1 Hz pacing. A separate
hardware/browser baseline is required before attributing this to scene cost or
setting an interaction budget. Raw observations are in `brain-5000-frames.json`
and `brain-overview-load.json`.

The dev-page idle observation (`idle-metrics.json`) spans 51.912 s, with 20.8 ms
script time and 282.628 ms total task time. Heap use at its end was 16,464,956
bytes. This is whole-page development instrumentation, not renderer-only cost.
Load memory snapshots include prior-page collection and HMR; they cannot prove
retained GPU memory or establish a leak slope. The truncated `camera-trace.json`
attempt is invalid and is excluded from conclusions.

Brain's coalesced activity envelopes identify project and family, not individual
agents. Encoding 100 invented agent IDs into them would not qualify 100 agents.
Temporal 100-agent/session, nested-branch, gap, and real PR #743 evidence belongs
to the complementary Loom qualification. This Brain report does not claim those
cases were rendered here.

## Verification and decision boundary

`npm test -- src/viz/graph src/workspaces/brain`: 17 files, 141 tests passed.
The later exact-event pivot regression passed 9 tests across two files; the
bounded-count wording passed 7 SignalPanel tests. Shared navigation regression
plus scope/palette suites passed 71 tests. Dashboard typecheck passed after the
navigation changes. Browser checks are the direct behavioral evidence above,
not a substitute for real workload provenance.

The demonstrated defects were corrected with existing Sigma/Canvas and DOM
capabilities. They do not justify a production engine cutover. The external
R3F/Three and Pixi comparison below remains separate from production code. Rich body,
curve/depth fidelity and large-density legibility remain material evaluation
criteria; successful compilation or raw draw counts do not establish plate
parity. No final engine winner or full issue closure is claimed here.


## Isolated candidate comparison

The comparison at `candidates/comparison.md` and its adjacent exact
`package-lock.json` uses React 19.2.8, R3F 9.7.0, Three 0.186.0, PixiJS 8.20.1,
Sigma 3.0.3 and Graphology 0.26.0. The exact R3F peer range accepts React/React DOM
`>=19 <19.3` and Three `>=0.156`; the isolated Rsbuild 1.7.6 build passed.
No candidate dependencies were added to the product.

All three draw the same authored eight-node scene and a 5,000-point authored
load field with 499 explicit relations. They use the same stable IDs and DOM
fallback. This is not the product's ForceAtlas2 layout and cannot demonstrate
that an engine improves that layout's density. The prototype Sigma view uses
vanilla circles/straight edges; the product already has its own smooth glow and
curved dendrites. Richer custom Pixi/R3F prototype code therefore cannot establish
an engine fidelity advantage.

| Runtime | 5,000-point mount | Pick-to-DOM commit | Completed cleanup |
| --- | ---: | ---: | ---: |
| Sigma 3.0.3 | 128.0 ms | 2.7 ms | 12.9 ms |
| R3F 9.7.0 / Three 0.186.0 | 158.9 ms | 4.3 ms | Not measured at this density |
| PixiJS 8.20.1 | 371.9 ms | 5.2 ms | 25.5 ms |

These are single-run observations in Chrome 152. Mount includes an animation
frame boundary. Shared-page heap and the 1 Hz browser pacing cannot rank engine
memory or playback. Every small-scene pointer pick resolved `project-0000`; the
shared keyboard fallback resolved `project-0001`. Completed cleanups left zero
canvases. The Pixi prototype uses CPU nearest-point picking, so it does not
qualify a GPU-picking implementation. Capture pairs are
`candidates/captures/{sigma,r3f,pixi}-{8,5000}.jpg`.

Decision: retain Sigma as the single current production runtime. The candidate
spike proves viable browser material-authoring alternatives, not a warranted
cutover. Blender can produce reference art or textures; interaction remains a
DOM/WebGL responsibility. The product's measured main-thread stall is in its
layout algorithm, not evidence that its renderer needs replacement.

## Hermetic production baseline and root cause

A separate repository test-browser run used the exact `7b5dd30f3` production
bundle, Chromium 149.0.7827.55 at 1440×1000, AMD EPYC 7742 with 96 visible CPUs.
Host load was approximately 64 during the run. Its empty-page frame baseline
was median 16.7/p95 16.8 ms, separating it from the connected-browser pacing
above. Raw measurements/captures are copied to `hermetic-before/` in the artifact
directory (original run: `/fast/tmp/td-graph-headless/`).

| Scene during 20,000-envelope stream | Frame median / p95 / max | Observed long tasks | JS heap before / after |
| --- | --- | --- | --- |
| 5,000 symbols, 4,999 relations | 16.7 / 16.8 / 16.8 ms | None during the burst | 105.6 / 109.3 MB |
| 29-project overview | 16.7 / 33.4 / 150 ms | Eight, 51–148 ms | 42.9 / 53.3 MB |

Both measured connections received all 20,000 envelopes. The overview's exact
terminal envelope was selected in the UI and resolved `tracedecay`; retention
remained 64. Aggregate project events are not symbol-level activity, so the
scoped graph is a static scene during this load.

The initial 5,000-symbol navigation took 12.69 seconds. A CPU sample attributes
6,738.6 ms self time to ForceAtlas2 `iterate`, versus approximately 446 ms in a
React frame and 238 ms in canvas measurement. Barnes–Hut was already enabled.
`5000-load-before.cpuprofile` and `load-cpu-summary.json` contain the observations.
The fix in `cfc6477ee` moves the same bounded calculation into a module worker;
it does not raise a limit, reduce iterations, or alter measured coordinates.
Cancellation terminates the worker, malformed/stale coordinates cannot replace
the active scene, and a pending state preserves the exact DOM data view.
Final geometry parity and worker cancellation/error behavior pass focused tests;
the browser comparison must distinguish time to usable DOM from time to final
geometry rather than disguise unchanged computation time.

The initial 1.61-second zoom measurement included expensive automation locator
resolution. A corrected baseline resolves coordinates before timing a trusted
pointer event: locator resolution alone took 787 ms; pointerdown to second
animation frame was 14, 401, and 16.5 ms over three samples. Median 16.5 ms does
not erase the 401 ms outlier. These measure first response, not completion of
the camera animation. A browser CPU sample also attributes roughly 796 ms to
injected accessibility/role lookup, so no renderer change is justified solely
by the original automation end-to-end number.


## Worker verification on the same production fixture

The final controlled run compares production `7b5dd30f3` with `cfc6477ee`, in the
same Chromium 149 browser configuration and 5,000-symbol/4,999-relation fixture.
Source, iteration count, and geometry are unchanged. The actual module worker
loads from the built bundle. Artifacts: `hermetic-worker/measurements.json`,
`hermetic-worker/worker-pending.png`, and the two settled screenshots.

| Observation | Synchronous baseline | Worker |
| --- | ---: | ---: |
| Usable DOM, responsiveness probe | 896 ms | 915 ms |
| Completed canvas, same probe | 7,988 ms | 8,140 ms |
| Main-thread force calculation task | 6,260 ms | Absent |
| Frame intervals while calculating | 2 samples; 7,066 ms gap | 371 samples; median/p95 16.7 ms |
| Final main-thread installation task | 822 ms | 836 ms |

The improvement is removal of the multi-second UI freeze, not faster final
geometry. The worker run still records a maximum 967 ms frame gap around final
installation and smaller 83/143 ms tasks. That installation cost and the dense
5,000-symbol overview remain limitations; they are not hidden by the frame p95.

The pending screenshot visibly identifies calculation and leaves the exact
symbol list available. Camera controls are disabled until the scene is ready.
A trusted navigation click terminated the pending worker in 107 ms; the worker
closed and no stale canvas appeared after leaving the view. The test inspected
actual worker lifetime, not only a mocked abort callback.

Both baseline and worker test connections received all 20,000 SSE envelopes.
The scoped graph stays static because aggregate project activity cannot invent
symbol activity. Their burst frame p95 values were 16.8/16.7 ms; no long task was
observed during either burst. Three CPU-profiled trusted zoom samples were
23.7/24.7/29.3 ms before and 37.7/32.4/27.1 ms after; the earlier 401 ms outlier did
not recur. This does not prove absence of outliers outside the sample, and the
measurement remains first response rather than completed camera animation.

The worker slice's typecheck and focused behavior tests passed, including real
ForceAtlas2 exact-coordinate parity, measured-field bypass, malformed results,
worker failure, unmount cancellation, and late results after a topology change.
The built-browser check supplies the separate evidence that the real worker
bundle and cancellation path operate correctly.
