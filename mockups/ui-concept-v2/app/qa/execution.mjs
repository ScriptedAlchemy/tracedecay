import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { chromium } from "playwright";

const baseUrl = process.env.BASE_URL ?? "http://127.0.0.1:5195";
const pack = JSON.parse(readFileSync(new URL("../src/data/pack-index.json", import.meta.url)));
const target = pack.sessions.find(
  (session) => session.parentId && pack.sessions.some((candidate) => candidate.id === session.parentId),
);
const stale = pack.sessions.find((session) => session.id !== target?.id);
const event = pack.loomEvents.find(
  (candidate) => candidate.sessionId !== target?.id && pack.sessions.some((session) => session.id === candidate.sessionId),
);

assert(target, "The snapshot needs one recorded parent edge for execution QA");
assert(stale, "The snapshot needs a second session for stale-state QA");
assert(event, "The snapshot needs one event with a recorded owning session for execution QA");

function percent(style, property) {
  const match = style.match(new RegExp(`${property}: ([\\d.]+)%`));
  assert(match, `Missing ${property} percentage in ${style}`);
  return Number(match[1]);
}

const browser = await chromium.launch();
try {
  const context = await browser.newContext({
    viewport: { width: 1600, height: 1000 },
    reducedMotion: "reduce",
  });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));

  await page.goto(baseUrl);
  await page.evaluate(() => {
    sessionStorage.setItem("td:view:snapshot:sessions.camera", JSON.stringify({}));
    sessionStorage.setItem("td:view:snapshot:sessions.camera-history", JSON.stringify([{}]));
  });
  const malformedCameraUrl = new URL(baseUrl);
  malformedCameraUrl.searchParams.set("surface", "sessions");
  await page.goto(malformedCameraUrl.href);
  const malformedBack = page.getByRole("button", { name: "BACK", exact: true });
  assert.equal(await malformedBack.isDisabled(), true, "Invalid history entries must not enable Camera Back");
  await malformedBack.evaluate((button) => button.click());
  await page.locator(".sn-root").waitFor();
  assert.doesNotMatch(await page.locator(".sn-brush").getAttribute("style"), /NaN|undefined/);
  assert.deepEqual(errors, []);
  await page.evaluate(() => {
    sessionStorage.removeItem("td:view:snapshot:sessions.camera");
    sessionStorage.removeItem("td:view:snapshot:sessions.camera-history");
  });
  console.log("PASS Sessions rejects malformed persisted camera and camera-history values");

  const fixtureSessionUrl = new URL(baseUrl);
  fixtureSessionUrl.searchParams.set("surface", "sessions");
  fixtureSessionUrl.searchParams.set("data", "fixture");
  fixtureSessionUrl.searchParams.set("session", target.id);
  await page.goto(fixtureSessionUrl.href);
  assert.match(await page.locator(".sn-mode").innerText(), /FIXTURE · SYNTHETIC SESSION SPINE/);
  assert.match(
    await page.getByLabel("Selected session provenance inspector").innerText(),
    /SOURCE AVAILABILITY MATRIX[\s\S]*TRANSCRIPT[\s\S]*PAGINATION[\s\S]*REDACTION[\s\S]*LINKS/i,
  );
  assert.match(await page.locator(".sn-transcript-fallback").innerText(), /EXACT EVENT FALLBACK[\s\S]*not ingested/);
  await page.getByLabel("Search session index (not transcript FTS)").fill("");
  await page.locator(".sn-table tbody tr").nth(1).waitFor();
  await page.locator(".sn-table tbody tr").first().focus();
  await page.keyboard.press("ArrowDown");
  assert.equal(
    await page.locator(":focus").getAttribute("data-session-id"),
    await page.locator(".sn-table tbody tr").nth(1).getAttribute("data-session-id"),
  );

  await page.evaluate((sessionId) => {
    sessionStorage.setItem("td:view:snapshot:sessions.selection", JSON.stringify(sessionId));
    sessionStorage.setItem("td:view:snapshot:sessions.query", JSON.stringify(sessionId));
    sessionStorage.setItem("td:view:snapshot:sessions.detail", JSON.stringify(sessionId));
  }, stale.id);

  const sessionUrl = new URL(baseUrl);
  sessionUrl.searchParams.set("surface", "sessions");
  sessionUrl.searchParams.set("session", target.id);
  await page.goto(sessionUrl.href);
  const inspector = page.getByLabel("Selected session provenance inspector");
  await inspector.waitFor();
  const inspectorText = await inspector.innerText();
  assert.match(inspectorText, new RegExp(target.id));
  assert.match(inspectorText, /Lifecycle\s+unavailable — not recorded[\s\S]*Source status\s+bodies unavailable/);
  assert.equal(await page.locator(`tr[aria-selected=true] [title="${target.id}"]`).count(), 1);

  await page.getByRole("button", { name: "7D", exact: true }).click();
  const brush = page.locator(".sn-brush");
  const beforeZoom = await brush.getAttribute("style");
  const canvas = page.locator(".sn-chart canvas");
  const canvasBox = await canvas.boundingBox();
  assert(canvasBox, "Sessions timeline canvas must be visible");
  await page.mouse.move(canvasBox.x + canvasBox.width * 0.7, canvasBox.y + canvasBox.height / 2);
  await page.mouse.wheel(0, -500);
  await page.waitForTimeout(50);
  const afterZoom = await brush.getAttribute("style");
  assert(beforeZoom && afterZoom);
  assert(percent(afterZoom, "width") < percent(beforeZoom, "width"), "Wheel zoom must narrow the temporal brush");

  await page.mouse.move(canvasBox.x + canvasBox.width * 0.55, canvasBox.y + canvasBox.height / 2);
  await page.mouse.down();
  await page.mouse.move(canvasBox.x + canvasBox.width * 0.4, canvasBox.y + canvasBox.height / 2, { steps: 4 });
  await page.mouse.up();
  const afterPan = await brush.getAttribute("style");
  assert(afterPan && percent(afterPan, "left") !== percent(afterZoom, "left"), "Pointer pan must move the temporal brush");

  await page.getByRole("button", { name: "BACK", exact: true }).click();
  assert.equal(await brush.getAttribute("style"), afterZoom, "Camera Back must restore the pre-pan brush");
  console.log("PASS Sessions exact session arrival overrides stale state and continuous camera zoom/pan/back works");

  const sessionSearch = page.getByLabel("Search session index (not transcript FTS)");
  await sessionSearch.fill("");
  const alternateId = await page.locator("tr[data-session-id]").evaluateAll((rows, currentId) =>
    rows.map((row) => row.getAttribute("data-session-id")).find((id) => id && id !== currentId) ?? null,
  target.id);
  assert(alternateId, "Sessions route QA needs a second visible exact row");
  await page.locator(`tr[data-session-id="${alternateId}"]`).click();
  let selectedRoute = new URL(page.url());
  assert.equal(selectedRoute.searchParams.get("session"), alternateId);
  assert.equal(selectedRoute.searchParams.has("event"), false);
  assert.match(await inspector.innerText(), new RegExp(alternateId));
  await page.reload();
  selectedRoute = new URL(page.url());
  assert.equal(selectedRoute.searchParams.get("session"), alternateId);
  assert.match(await page.getByLabel("Selected session provenance inspector").innerText(), new RegExp(alternateId));
  assert.equal(await page.locator(`tr[aria-selected=true] [title="${alternateId}"]`).count(), 1);
  console.log("PASS Sessions exact row selection replaces the route and survives reload");

  const loomArrivalUrl = new URL(baseUrl);
  loomArrivalUrl.searchParams.set("surface", "sessions");
  loomArrivalUrl.searchParams.set("loom_pivot", "1");
  loomArrivalUrl.searchParams.set("loom_source", "mac");
  loomArrivalUrl.searchParams.set("loom_target_session", target.id);
  loomArrivalUrl.searchParams.set("loom_return", "/?surface=loom&loom_source=mac");
  await page.goto(loomArrivalUrl.href);
  await page.getByLabel("Search session index (not transcript FTS)").fill("");
  await page.locator(`tr[data-session-id="${alternateId}"]`).click();
  const retainedPivot = new URL(page.url());
  assert.equal(retainedPivot.searchParams.get("session"), alternateId);
  assert.equal(retainedPivot.searchParams.get("loom_target_session"), alternateId);
  assert.equal(retainedPivot.searchParams.get("loom_return"), "/?surface=loom&loom_source=mac");
  await page.reload();
  assert.match(await page.getByLabel("Selected session provenance inspector").innerText(), new RegExp(alternateId));
  console.log("PASS Sessions exact selection retains Loom source/back context with the new route identity");

  await page.evaluate((sessionId) => {
    sessionStorage.setItem("td:view:snapshot:sessions.selection", JSON.stringify(sessionId));
    sessionStorage.setItem("td:view:snapshot:sessions.query", JSON.stringify(sessionId));
    sessionStorage.setItem("td:view:snapshot:sessions.detail", JSON.stringify(sessionId));
  }, stale.id);
  const eventUrl = new URL(baseUrl);
  eventUrl.searchParams.set("surface", "sessions");
  eventUrl.searchParams.set("event", event.id);
  await page.goto(eventUrl.href);
  const eventInspector = page.getByLabel("Selected session provenance inspector");
  await eventInspector.waitFor();
  assert.match(await eventInspector.innerText(), new RegExp(event.sessionId));
  assert.equal(await page.locator(`tr[aria-selected=true] [title="${event.sessionId}"]`).count(), 1);
  console.log("PASS Sessions exact event arrival resolves its owning session over stale state");

  const missingUrl = new URL(baseUrl);
  missingUrl.searchParams.set("surface", "sessions");
  missingUrl.searchParams.set("session", "missing-session");
  await page.goto(missingUrl.href);
  assert.match(await page.locator(".sn-arrival-unavailable").innerText(), /No session has been substituted/);
  assert.equal(await page.getByLabel("Selected session provenance inspector").count(), 0);
  assert.equal(await page.locator("tr[aria-selected=true]").count(), 0);
  console.log("PASS Sessions unavailable arrival remains explicit and selects no substitute");

  const agentsUrl = new URL(baseUrl);
  agentsUrl.searchParams.set("surface", "agents");
  await page.goto(agentsUrl.href);
  assert.match(await page.locator(".ag-topo").innerText(), /48 unlinked/);
  assert.match(await page.locator(".ag-risk-unavailable").innerText(), /issue \/ risk filter unavailable.*failure authority was not captured/);
  await page.locator(".ag-link.is-parentage").first().focus();
  await page.keyboard.press("Enter");
  assert.match(await page.getByLabel("Selected relation").innerText(), /RECORDED PARENT EDGE[\s\S]*parent_session_id · EXACT/);
  console.log("PASS Agents snapshot preserves 48 unlinked sessions and typed parentage authority");

  await page.setViewportSize({ width: 793, height: 700 });
  const fixtureUrl = new URL(baseUrl);
  fixtureUrl.searchParams.set("surface", "agents");
  fixtureUrl.searchParams.set("data", "fixture");
  await page.goto(fixtureUrl.href);
  for (const [label, expected] of [
    ["UNIQUE AGENTS", "121"],
    ["RELATIONS", "132"],
    ["EVIDENCE GAPS", "4"],
  ]) {
    assert.match(await page.locator(".ag-chip").filter({ hasText: label }).innerText(), new RegExp(expected));
  }

  const spatialMap = page.getByRole("button", { name: /Topology minimap/ });
  await spatialMap.waitFor();
  assert.equal(await page.getByRole("navigation", { name: "Branch navigator" }).count(), 1);
  assert.equal(await page.locator(".ag-spatial-minimap .ag-mini-node").count(), 7);
  const pinnedFocus = page.getByRole("button", { name: "PINNED 0", exact: true });
  assert.equal(await pinnedFocus.isDisabled(), true);
  const selectedBeforeFocus = await page.locator(".ag-dense-node.is-on").getAttribute("aria-label");
  await page.getByRole("button", { name: "Pin INGEST ROUTING", exact: true }).click();
  await page.getByRole("button", { name: "Pin MEMORY HYDRATION", exact: true }).click();
  await page.getByRole("button", { name: "PINNED 2", exact: true }).click();
  assert.equal(await page.locator(".ag-dense-node.is-pinned").count(), 2);
  assert.equal(await page.locator(".ag-dense-node.is-dim").count(), 4);
  assert.equal(await page.locator(".ag-dense-node.is-on").getAttribute("aria-label"), selectedBeforeFocus);
  assert.match(await page.locator(".ag-topo-head .meta").innerText(), /40\/121 exact rows · 2 pinned branches/);
  await page.getByRole("button", { name: "EXACT ROWS", exact: true }).click();
  assert.equal(await page.getByRole("table", { name: "Exact fixture agents" }).getByRole("row").count(), 40);
  await page.getByRole("button", { name: "EXACT ROWS", exact: true }).click();
  await page.getByRole("button", { name: "PINNED 2", exact: true }).click();

  const issueFocus = page.getByRole("button", { name: "ISSUES 14", exact: true });
  assert.match(await issueFocus.getAttribute("title"), /No downstream risk is inferred/);
  await issueFocus.click();
  assert.match(await page.locator(".ag-topo-head .meta").innerText(), /14\/121 exact rows · ISSUES = failed \/ ambiguous \/ missing parent/);
  await page.getByRole("button", { name: "EXACT ROWS", exact: true }).click();
  assert.equal(await page.getByRole("table", { name: "Exact fixture agents" }).getByRole("row").count(), 14);
  await page.getByRole("button", { name: "EXACT ROWS", exact: true }).click();
  await issueFocus.click();
  console.log("PASS Agents pins and issue focus preserve topology positions and expose exact source-backed rows");

  const bundles = page.locator(".ag-dense-node.is-bundle");
  await bundles.first().waitFor();
  assert.equal(await bundles.count(), 6);
  const bounds = await bundles.evaluateAll((nodes) => nodes.map((node) => {
    const box = node.getBoundingClientRect();
    return { top: box.top, bottom: box.bottom };
  }));
  const viewport = page.viewportSize();
  assert(viewport, "Agents fixture viewport must be available");
  assert(bounds.every((box) => box.top >= 0 && box.bottom <= viewport.height), "FIT must keep all six outcome bundles visible");

  await bundles.first().focus();
  await page.keyboard.press("Enter");
  assert.equal(await page.getByRole("button", { name: "branch", exact: true }).getAttribute("aria-pressed"), "true");
  const branchBundle = page.locator(".ag-dense-node.is-bundle").first();
  const branchBundleLabel = await branchBundle.getAttribute("aria-label");
  assert(branchBundleLabel?.startsWith("Select "));
  await branchBundle.focus();
  await page.keyboard.press("Enter");
  assert.equal(await page.getByRole("button", { name: "agent", exact: true }).getAttribute("aria-pressed"), "true");
  assert.equal(await page.locator(".ag-dense-node.is-on").getAttribute("aria-label"), branchBundleLabel);
  const parentReference = page.getByLabel("Selected relation").locator(".ag-block").filter({ hasText: "PARENT REFERENCE" });
  assert.equal(await parentReference.locator(".ag-row").getAttribute("class"), "ag-row");
  assert.equal(await parentReference.locator(".r").getAttribute("class"), "r");

  const branchSearch = page.getByRole("searchbox", { name: "Search branches" });
  await branchSearch.fill("delivery-19");
  const searchRows = page.getByRole("table", { name: "Exact fixture agents" }).getByRole("row");
  assert.equal(await searchRows.count(), 1);
  assert.match(await searchRows.first().innerText(), /delivery-19[\s\S]*missing:release-agent/);
  await searchRows.first().click();
  const gapInspector = page.getByLabel("Selected relation");
  assert.match(await gapInspector.innerText(), /delivery-19[\s\S]*missing:release-agent/);
  assert.match(await gapInspector.locator(".ag-block").filter({ hasText: "PARENT REFERENCE" }).locator(".ag-row").getAttribute("class"), /is-abs/);
  await page.getByRole("tab", { name: "Topology", exact: true }).click();
  await branchSearch.fill("");

  await page.getByRole("button", { name: "event", exact: true }).click();
  await page.locator(".ag-dense-edge.is-parentage").first().focus();
  await page.keyboard.press("Enter");
  assert.match(await page.getByLabel("Selected relation").innerText(), /RECORDED PARENT EDGE[\s\S]*kind\s+parentage[\s\S]*grade\s+EXACT/);
  await page.goto(baseUrl);
  await page.evaluate(() => sessionStorage.clear());
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(fixtureUrl.href);
  await page.locator(".ag-dense-node.is-bundle").first().focus();
  await page.keyboard.press("Enter");
  const narrowField = page.locator(".ag-dense-field");
  const narrowGeometry = await narrowField.evaluate((field) => ({
    bodyWidth: document.documentElement.scrollWidth,
    viewportWidth: innerWidth,
    clientWidth: field.clientWidth,
    scrollWidth: field.scrollWidth,
    labelSize: Number.parseFloat(getComputedStyle(field.querySelector(".ag-dense-node .name")).fontSize),
  }));
  assert.equal(narrowGeometry.bodyWidth, narrowGeometry.viewportWidth);
  assert(narrowGeometry.scrollWidth > narrowGeometry.clientWidth, "Narrow topology must pan locally instead of shrinking labels");
  assert(narrowGeometry.labelSize >= 10, "Narrow topology labels must retain their authored readable size");
  const narrowMap = page.getByRole("button", { name: /Topology minimap/ });
  assert.equal(await narrowMap.isVisible(), true, "The topology minimap must remain available on narrow layouts");
  const selectedBeforeMap = await page.locator(".ag-dense-node.is-on").getAttribute("aria-label");
  const viewportBeforePan = Number(await page.locator(".ag-mini-viewport").getAttribute("x"));
  await narrowField.evaluate((field) => { field.scrollLeft = 120; });
  await page.waitForTimeout(50);
  const viewportAfterPan = Number(await page.locator(".ag-mini-viewport").getAttribute("x"));
  assert(viewportAfterPan > viewportBeforePan, "The minimap viewport must track local topology panning");
  await narrowField.evaluate((field) => { field.scrollLeft = 0; });
  const narrowMapBounds = await narrowMap.boundingBox();
  assert(narrowMapBounds, "The narrow topology minimap must have clickable bounds");
  await page.mouse.click(narrowMapBounds.x + narrowMapBounds.width * .9, narrowMapBounds.y + narrowMapBounds.height * .55);
  const pointerPanned = await narrowField.evaluate((field) => field.scrollLeft);
  assert(pointerPanned > 0, "Pointer activation on the minimap must pan the topology camera");
  await narrowMap.focus();
  await page.keyboard.press("Enter");
  assert((await narrowField.evaluate((field) => field.scrollLeft)) < pointerPanned, "Keyboard activation must center the selected topology node");
  assert.equal(await page.locator(".ag-dense-node.is-on").getAttribute("aria-label"), selectedBeforeMap);
  assert.deepEqual(errors, []);
  console.log("PASS Agents dense counts, FIT, bundle drill-down, live search, typed parentage, and narrow map-guided panning");
} finally {
  await browser.close();
}
