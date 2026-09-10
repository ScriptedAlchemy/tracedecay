import { chromium } from "playwright";

const baseUrl = process.env.BASE_URL ?? "http://127.0.0.1:5173";
const browser = await chromium.launch({
  args: [
    "--no-sandbox",
    "--force-device-scale-factor=1",
    "--use-gl=angle",
    "--use-angle=swiftshader-webgl",
    "--enable-unsafe-swiftshader",
  ],
});
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
let fail = 0;

// --- 2D scoped: hover isolates locally, scope/URL/register untouched -------
await page.goto(`${baseUrl}/?view=scoped&dim=2d&data=fixture`, { waitUntil: "networkidle" });
await page.waitForTimeout(2000);
const ap = await page.locator("section.aperture").boundingBox();
const regBefore = await page.locator(".register h1").innerText();
const urlBefore = page.url();
// sessions hub sits near (0.44w, 0.5h) of the aperture
await page.mouse.move(ap.x + ap.width * 0.44, ap.y + ap.height * 0.5, { steps: 4 });
await page.waitForTimeout(700);
const regAfter = await page.locator(".register h1").innerText();
if (regBefore !== regAfter || page.url() !== urlBefore) {
  fail++;
  console.log(`FAIL 2d scoped hover changed scope: "${regBefore}" -> "${regAfter}" url ${page.url()}`);
} else {
  console.log("ok   2d scoped hover keeps scope + URL");
}

// --- 3D scoped on a non-default project: hover must not remount tracedecay -
await page.goto(`${baseUrl}/?view=scoped&scope=proj_e19f6f383c982ea8&dim=3d&data=fixture`, {
  waitUntil: "networkidle",
});
await page.waitForTimeout(3600);
const reg3dBefore = await page.locator(".register h1").innerText();
// Grid-scan until the inspect readout confirms a real node hover. The scene
// slowly rotates, so fixed coordinates can miss every node; a vacuous pass
// here would never exercise the onFocus path this test exists to guard.
const idle = "hover a node · inspect only";
let inspect3d = null;
scan: for (let gy = 3; gy <= 8; gy++) {
  for (let gx = 2; gx <= 10; gx++) {
    await page.mouse.move(ap.x + (ap.width * gx) / 12, ap.y + (ap.height * gy) / 12, { steps: 2 });
    await page.waitForTimeout(90);
    const t = (await page.locator(".nl-inspect").innerText()).trim();
    if (t && t !== idle) {
      inspect3d = t;
      break scan;
    }
  }
}
await page.waitForTimeout(600);
const reg3dAfter = await page.locator(".register h1").innerText();
const url3d = page.url();
if (inspect3d == null) {
  fail++;
  console.log("FAIL 3d scoped hover: grid scan never hit a constellation node (inspect stayed idle)");
} else if (
  !reg3dBefore.includes("ZeroFS") ||
  !reg3dAfter.includes("ZeroFS") ||
  !url3d.includes("proj_e19f6f383c982ea8")
) {
  fail++;
  console.log(`FAIL 3d scoped hover: register "${reg3dBefore}" -> "${reg3dAfter}" url ${url3d}`);
} else {
  console.log(`ok   3d scoped hover hit "${inspect3d}" and kept ZeroFS scope + URL`);
}

await browser.close();
process.exit(fail ? 1 : 0);
