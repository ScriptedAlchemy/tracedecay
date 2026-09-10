import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { chromium } from "playwright";

const browser = await chromium.launch({
  ...(process.env.CHROMIUM_EXECUTABLE_PATH
    ? { executablePath: process.env.CHROMIUM_EXECUTABLE_PATH }
    : existsSync(chromium.executablePath()) ? {} : { channel: "chrome" }),
});
try {
  const page = await browser.newPage({ viewport: { width: 1586, height: 992 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const base = process.env.BASE_URL ?? "http://127.0.0.1:5173";
  const visit = (params) => page.goto(`${base}/?${new URLSearchParams({ surface: "delivery", ...params })}`);

  await visit({ data: "snapshot", state: "03" });
  await page.locator('.dl-tracking-unavailable,.dl-branch-workspace').waitFor();
  assert.equal(await page.getByLabel("Delivery view").count(), 0, "Snapshot must not offer fixture views it cannot render");
  if (await page.getByRole("status", { name: "Tracked branch scope unavailable" }).count()) {
    assert.equal(await page.locator(".dl-branch-card,.dl-branch-row").count(), 0, "Unadmitted account results must not seed the branch view");
    await page.getByRole("button", { name: "Explore authored Delivery design", exact: true }).click();
    console.log("PASS unavailable index authority has no primary branch results");
  } else {
    const rows = page.locator(".dl-branch-row");
    const count = await rows.count();
    assert.ok(count > 0, "Available branch source must render actual branches");
    assert.ok(await page.locator(".dl-branch-card,.dl-change-footprint").count() > 0);
    if (await page.locator(".dl-change-file").count()) {
      await page.evaluate(() => sessionStorage.setItem("td:view:snapshot:delivery.branches.selected", JSON.stringify("no-longer-in-export")));
      await page.reload();
      await page.locator(".dl-change-file").first().click();
      assert.equal(await page.locator(".dl-change-file").first().getAttribute("aria-pressed"), "true", "A removed saved branch must not disable files in the visible branch");
      await page.getByRole("link", {name:"Open exact file revision ↗"}).waitFor();
    }
    const lastTitle = await rows.last().locator("strong").innerText();
    await rows.last().click();
    assert.equal(await page.locator(".dl-branch-inspector h2").innerText(), lastTitle);
    const selectedUrl = page.url();
    assert.ok(new URL(selectedUrl).searchParams.get("branch"));
    await page.reload();
    assert.equal(await page.locator(".dl-branch-inspector h2").innerText(), lastTitle);
    await page.getByLabel("Search tracked branches").fill("no-such-indexed-branch-qa");
    assert.equal(await rows.count(), 0);
    assert.equal(await page.locator(".dl-branch-card").count(), 0);
    await page.goto(selectedUrl);
    assert.equal(await page.getByLabel("Search tracked branches").inputValue(), "", "A direct branch link reveals its target despite a saved filter");
    assert.equal(await page.locator(".dl-branch-inspector h2").innerText(), lastTitle);
    await page.getByLabel("Search tracked branches").fill("no-such-indexed-branch-qa");
    await page.getByRole("button", { name: "Clear filters", exact: true }).click();
    assert.equal(await rows.count(), count);
    const projectSelect = page.getByLabel("Filter tracked branches by project");
    const project = await projectSelect.locator("option").nth(1).getAttribute("value") ?? await projectSelect.locator("option").nth(1).innerText();
    await projectSelect.selectOption({ label: project });
    assert.ok(await rows.count() > 0);
    for(const name of await rows.locator("span").allTextContents()) assert.equal(name, project);
    await projectSelect.selectOption("");
    const relation = page.locator(".dl-branch-relations button").first();
    if(await relation.count()) {
      await relation.click();
      await page.getByRole("link", { name: "Open exact relationship source ↗" }).waitFor();
      await page.getByRole("button", { name: "Return to selected branch" }).click();
    }
    const files = page.locator(".dl-change-file");
    if (await files.count()) {
      const originalCount = await files.count();
      await page.getByRole("button", {name:/^Focus directory /}).first().click();
      assert.ok(await files.count() > 0);
      await files.first().focus(); await page.keyboard.press("Enter");
      await page.getByRole("link", {name:"Open exact file revision ↗"}).waitFor();
      const fileHeading = await page.locator(".dl-branch-inspector h2").innerText();
      await page.reload();
      assert.equal(await page.locator(".dl-branch-inspector h2").innerText(), fileHeading, "Reload preserves the selected file for the same branch");
      await page.getByRole("button", {name:"Back to indexed branch", exact:true}).click();
      await page.getByRole("button", {name:"Back to changed files", exact:true}).click();
      assert.equal(await files.count(), originalCount);
    }
    await page.getByText("Exact tracking and index proof", { exact: true }).click();
    assert.ok((await page.locator(".dl-branch-inspector details[open]").innerText()).includes("source_oid"));
    if (count === 1) {
      await page.setViewportSize({ width: 1263, height: 931 });
      const evidenceBounds = await page.locator(".dl-branch-inspector").boundingBox();
      assert.ok(evidenceBounds && evidenceBounds.y < 650, "Single-branch evidence should be visible below the compact scene");
      await rows.first().focus();
      await page.keyboard.press("Enter");
      assert.equal(await page.locator(".dl-branch-inspector h2").innerText(), lastTitle);
      await page.setViewportSize({ width: 1586, height: 992 });
    }
    await page.getByRole("button", { name: "Explore authored Delivery design", exact: true }).click();
    console.log("PASS admitted branch rows/cards, project/search, indexed-source selection and return");
  }
  await page.waitForURL((url) => url.searchParams.get("data") === "fixture");
  await page.getByText(/Authored design fixture/).waitFor();
  assert.equal(await page.getByLabel("Delivery view").locator("option").count(), 12);
  await page.getByLabel("Delivery view").selectOption("05");
  await page.waitForURL((url) => url.searchParams.get("state") === "05");

  await visit({ data: "fixture", state: "03" });
  await page.setViewportSize({width:1263,height:931});
  const umbrella = page.locator(".dl-field svg");
  const overview = await umbrella.getAttribute("viewBox");
  assert.equal(await page.getByRole("button", {name:/^Inspect fixture PR /}).count(),15);
  assert.equal(await page.getByRole("button", {name:/^Inspect fixture PR /}).locator("text").count(),15,"Every overview marker must visibly identify its PR");
  assert.equal(await page.getByRole("button",{name:"Inspect umbrella outcome",exact:true}).evaluate(root=>{
    const ring=root.querySelector("ellipse");const cx=Number(ring.getAttribute("cx")),cy=Number(ring.getAttribute("cy")),r=Number(ring.getAttribute("rx"));
    return [...root.querySelectorAll("text")].every(text=>{const q=text.getBBox();return [[q.x,q.y],[q.x+q.width,q.y],[q.x,q.y+q.height],[q.x+q.width,q.y+q.height]].every(([x,y])=>(x-cx)**2+(y-cy)**2<r*r);});
  }),true,"Outcome text must fit inside its actual ring");
  await page.getByRole("button", {name:"Inspect Rspack",exact:true}).locator("text").first().click();
  assert.notEqual(await umbrella.getAttribute("viewBox"), overview);
  await page.getByRole("button", {name:"Inspect fixture PR Rspack #18337",exact:true}).click();
  await page.locator(".dl-umbrella-detail h2").filter({hasText:"#18337"}).waitFor();
  assert.match(await page.locator(".dl-umbrella-breadcrumb").innerText(),/Example outcome \/ Rspack \/ #18337/);
  await page.getByRole("button", {name:"Back",exact:true}).click();
  await page.getByRole("button", {name:"Back",exact:true}).click();
  assert.equal(await umbrella.getAttribute("viewBox"),overview);
  await page.getByRole("button", {name:"Focus repository Rspack",exact:true}).focus();
  await page.keyboard.press("Enter");
  assert.notEqual(await umbrella.getAttribute("viewBox"),overview);
  await page.getByRole("button", {name:"Fit all",exact:true}).click();
  assert.equal(await umbrella.getAttribute("viewBox"),overview);
  await page.getByRole("button", {name:"Pan example right",exact:true}).click();
  assert.notEqual(await umbrella.getAttribute("viewBox"),overview);
  await page.getByRole("button", {name:"Back",exact:true}).click();
  assert.equal(await umbrella.getAttribute("viewBox"),overview);
  await page.getByRole("button", {name:"Inspect inferred membership Rspack",exact:true}).locator("circle").click();
  await page.locator(".dl-umbrella-detail h2").filter({hasText:"Inferred membership"}).waitFor();
  await page.setViewportSize({width:1586,height:992});

  await visit({ data: "fixture", state: "04" });
  const initialCamera = await page.locator(".dl-weave svg").getAttribute("viewBox");
  await page.getByRole("button", { name: "Zoom overview in", exact: true }).click();
  const zoomedCamera = await page.locator(".dl-weave svg").getAttribute("viewBox");
  assert.notEqual(zoomedCamera, initialCamera);
  await page.reload();
  assert.equal(await page.locator(".dl-weave svg").getAttribute("viewBox"), zoomedCamera);

  await visit({ data: "fixture", state: "06" });
  await page.getByRole("button",{name:/^Inspect episode:/}).first().click();
  const branchInspector=page.getByRole("complementary",{name:"Selected represented branch"});
  await branchInspector.locator("h2").waitFor();
  const firstBranch=await branchInspector.innerText();
  await page.locator("[popover]:popover-open").getByRole("button",{name:"Close episode details"}).click();
  await page.getByRole("button",{name:/^Inspect episode:/}).last().focus();
  await page.keyboard.press("Enter");
  assert.notEqual(await branchInspector.innerText(),firstBranch);
  await page.locator("[popover]:popover-open").getByRole("button",{name:"Close episode details"}).click();
  for(const width of [1586,1263]) {
    await page.setViewportSize({width,height:931});
    await visit({data:"fixture",state:"11"});
    const repository=page.getByRole("button",{name:/^Inspect local repository /}).first();
    await repository.click();
    await page.locator("[popover]:popover-open h2").waitFor();
    assert.match(await page.locator("[popover]:popover-open").innerText(),/Last-indexed freshness/);
    await page.getByRole("button",{name:"Back to local evidence"}).click();
    assert.equal(await repository.getAttribute("aria-pressed"),"true");
    const projection=page.getByRole("button",{name:/^Inspect projection /}).last();
    await projection.focus();await page.keyboard.press("Enter");
    assert.match(await page.locator("[popover]:popover-open").innerText(),/independent availability state/);
    await page.getByRole("button",{name:"Back to local evidence"}).click();
    assert.equal(await projection.getAttribute("aria-pressed"),"true");
  }
  await page.setViewportSize({width:1586,height:992});

  await visit({ data: "fixture", state: "12" });
  await page.getByRole("button", { name: "PATH TO OUTCOME", exact: true }).click();
  await page.getByRole("status").getByText(/No recorded integration/).waitFor();
  await page.locator('[aria-label="Outcome evidence unavailable"]').waitFor();

  await visit({ data: "fixture", workflow: "enrich-graph", run: "qa-run", task: "qa-task" });
  await page.getByRole("note").filter({ hasText: "Incoming workflow context" }).waitFor();
  await page.getByRole("button", { name: "Return to workflow", exact: true }).click();
  await page.waitForURL((url) => url.searchParams.get("surface") === "workflows");
  assert.equal(new URL(page.url()).searchParams.get("run"), "qa-run");
  assert.equal(new URL(page.url()).searchParams.get("task"), "qa-task");
  assert.deepEqual(errors, []);
  console.log("PASS Delivery admission boundary, all named fixture views, camera return, open outcome evidence and incoming workflow context");
} finally {
  await browser.close();
}
