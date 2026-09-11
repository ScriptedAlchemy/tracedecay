import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { chromium } from "playwright";

const browser = await chromium.launch({
  ...(process.env.CHROMIUM_EXECUTABLE_PATH
    ? { executablePath: process.env.CHROMIUM_EXECUTABLE_PATH }
    : existsSync(chromium.executablePath()) ? {} : { channel: "chrome" }),
});
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.addInitScript(() => {
    const getContext = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (type, ...args) {
      if (type === "webgl" || type === "webgl2" || type === "experimental-webgl") return null;
      return getContext.call(this, type, ...args);
    };
  });
  const base = process.env.BASE_URL ?? "http://127.0.0.1:5173";
  await page.goto(`${base}/?data=fixture`);
  await page.waitForURL((url) => url.searchParams.get("dim") === "2d");
  assert.equal(await page.getByRole("tab", { name: "Overview", exact: true }).isVisible(), true);
  console.log("PASS default opens plate-oriented 2D overview");
  for (const route of ["?data=fixture&dim=3d", "?data=fixture&view=firing-tree", "?data=fixture&view=neuron-lab"]) {
    await page.goto(`${base}/${route}`);
    await page.getByRole("heading", { name: "Brain renderer unavailable" }).waitFor();
    assert.equal(await page.getByRole("navigation", { name: "Workspaces" }).isVisible(), true);
    await page.getByText("Renderer error", { exact: true }).click();
    assert.match(await page.locator("[role=alert] pre").innerText(), /WebGL/);
    await page.getByRole("button", { name: "Open 2D overview", exact: true }).click();
    await page.getByRole("tab", { name: "Overview", exact: true }).waitFor();
    await page.waitForFunction(() => {
      const canvas = document.querySelector(".aperture canvas");
      return canvas?.width > 0 && canvas.getContext("2d").getImageData(0, 0, 1, 1).data[3] > 0;
    });
    assert.equal(new URL(page.url()).searchParams.get("dim"), "2d");
    await page.getByRole("tab", { name: "Hover", exact: true }).click();
    assert.equal(await page.getByRole("tab", { name: "Hover", exact: true }).getAttribute("aria-selected"), "true");
    await page.getByRole("button", { name: "3D", exact: true }).click();
    await page.getByRole("heading", { name: "Brain renderer unavailable" }).waitFor();
    await page.getByRole("navigation").getByRole("button", { name: /Knowledge/ }).click();
    await page.waitForURL((url) => url.searchParams.get("surface") === "knowledge");
    assert.equal(await page.getByRole("heading", { name: "Brain renderer unavailable" }).count(), 0);
    console.log(`PASS renderer recovery ${route || "default"}`);
  }
  await page.goto(`${base}/?data=fixture&view=overview&dim=2d`);
  await page.getByText("Render tuning", { exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".aperture canvas")?.width > 0);
  const original = await page.locator(".aperture canvas").evaluate((canvas) => canvas.toDataURL());
  const lightLevel = () => page.locator(".aperture canvas").evaluate((canvas) => {
    const pixels = canvas.getContext("2d").getImageData(0, 0, canvas.width, canvas.height).data;
    let total = 0;
    for (let i = 0; i < pixels.length; i += 4) total += pixels[i] + pixels[i + 1] + pixels[i + 2];
    return total;
  });
  const originalLight = await lightLevel();
  for (const name of ["Glow strength", "Particle density", "Branch width"]) {
    const control = page.getByRole("slider", { name, exact: true });
    await control.focus();
    await control.press("End");
    assert.equal(await control.inputValue(), "2");
    await page.waitForFunction((before) => document.querySelector(".aperture canvas").toDataURL() !== before, original);
    await page.getByRole("button", { name: "Reset tuning", exact: true }).click();
    assert.equal(await control.inputValue(), "1");
    await page.waitForFunction((before) => document.querySelector(".aperture canvas").toDataURL() === before, original);
    console.log(`PASS render tuning changes pixels and resets: ${name}`);
  }
  const glow = page.getByRole("slider", { name: "Glow strength", exact: true });
  await glow.focus();
  await glow.press("Home");
  assert.equal(await glow.inputValue(), "0");
  await page.waitForFunction((before) => document.querySelector(".aperture canvas").toDataURL() !== before, original);
  assert.ok(await lightLevel() < originalLight, "Zero glow must remove emission light");
  await page.getByRole("button", { name: "Reset tuning", exact: true }).click();
  await page.waitForFunction((before) => document.querySelector(".aperture canvas").toDataURL() === before, original);
  console.log("PASS zero glow removes emission light and resets");
  await page.getByRole("tab", { name: "Repo", exact: true }).click();
  assert.equal(await page.locator(".render-tuning").count(), 0);
  const repoCanvas = page.locator(".aperture canvas");
  const repoPixels = await repoCanvas.evaluate((canvas) => canvas.toDataURL());
  await page.getByRole("button", { name: "Zoom in", exact: true }).click();
  await page.waitForFunction((before) => document.querySelector(".aperture canvas").toDataURL() !== before, repoPixels);
  await page.getByRole("button", { name: "Fit", exact: true }).click();
  await page.waitForFunction((before) => document.querySelector(".aperture canvas").toDataURL() === before, repoPixels);
  const checkoutButton = page.getByRole("button", { name: "Inspect checkout ui-concept-first-party", exact: true });
  await checkoutButton.press("Enter");
  const details = page.getByRole("complementary", { name: "Checkout details" });
  assert.match(await details.innerText(), /Exported checkout registry/);
  assert.match(await details.innerText(), /unavailable/);
  assert.equal(await checkoutButton.getAttribute("aria-pressed"), "true");
  await page.getByRole("button", { name: "Close checkout details", exact: true }).click();
  const bounds = await repoCanvas.boundingBox();
  await repoCanvas.click({ position: { x: bounds.width * 0.27, y: bounds.height * 0.29 } });
  assert.equal(await details.getByRole("heading", { name: "redesign", exact: true }).isVisible(), true);
  await page.getByRole("button", { name: "Open project: tracedecay", exact: true }).click();
  await page.waitForURL((url) => url.searchParams.get("view") === "scoped");
  await page.keyboard.press("Escape");
  await page.waitForURL((url) => url.searchParams.get("view") === "overview");
  console.log("PASS repository zoom, keyboard and mesh picking, source detail and project scope");
  await page.getByRole("tab", { name: "Hover", exact: true }).click();
  const core = page.getByRole("button", { name: /^Inspect core, indexed mass/ });
  await core.focus();
  await page.getByRole("heading", { name: "core", exact: true }).waitFor();
  assert.equal(new URL(page.url()).searchParams.get("view"), "hover");
  await core.press("Enter");
  await page.waitForURL((url) => url.searchParams.get("view") === "scoped");
  assert.equal(await page.getByRole("heading", { name: "core", exact: true }).isVisible(), true);
  await page.keyboard.press("Escape");
  await page.waitForURL((url) => url.searchParams.get("view") === "overview");
  console.log("PASS caption keyboard inspection, scope and Escape");
  assert.match(await page.locator(".status").innerText(), /DATA\s+design fixture/);
  await page.goto(`${base}/?data=fixture&view=overview&dim=2d`, { waitUntil: "networkidle" });
  for (const [width, height] of [[1586, 992], [1280, 800]]) {
    await page.setViewportSize({ width, height });
    await page.waitForFunction(() => {
      const canvas = document.querySelector(".aperture canvas");
      return canvas.width === canvas.parentElement.clientWidth && canvas.height === canvas.parentElement.clientHeight;
    });
    await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.evaluate(() => document.fonts.ready);
    const collisions = await page.evaluate(() => {
      const labels = [...document.querySelectorAll(".label")];
      const obstacles = [...document.querySelectorAll(".signal, .brain-chips, .axis-x, .axis-y, .render-tuning")];
      const aperture = document.querySelector(".aperture").getBoundingClientRect();
      const intersect = (a, b) => a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
      return labels.flatMap((label, index) => {
        const box = label.getBoundingClientRect();
        const failures = [...labels.slice(index + 1), ...obstacles]
          .filter((other) => intersect(box, other.getBoundingClientRect()))
          .map((other) => `${label.textContent} overlaps ${other.className}`);
        if (box.left < aperture.left || box.right > aperture.right || box.top < aperture.top || box.bottom > aperture.bottom) failures.push(`${label.textContent} clipped`);
        return failures;
      });
    });
    assert.deepEqual(collisions, []);
    console.log(`PASS caption bounds and HUD isolation ${width}x${height}`);
  }
} finally {
  await browser.close();
}
