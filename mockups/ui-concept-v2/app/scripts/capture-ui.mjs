// Start the demo, then run: BASE_URL=http://127.0.0.1:5195 node scripts/capture-ui.mjs
import { chromium } from 'playwright';
import { mkdir, readdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const out = resolve(process.env.OUTPUT_DIR ?? `${root}/../screenshots`);
const base = process.env.BASE_URL ?? 'http://127.0.0.1:5195';
const plates = resolve(root, '..');
const captures = [];
const browser = await chromium.launch();
const errors = [];
const page = await browser.newPage({ reducedMotion: 'reduce' });
page.on('pageerror', error => errors.push(error.message));
const title = name => name.replace(/^\d+-/, '').replaceAll('-', ' ');
async function open(query, viewport = { width: 1586, height: 992 }) {
  await page.setViewportSize(viewport);
  await page.goto(`${base}/?${query}`);
  await page.getByRole('navigation', { name: 'Workspaces' }).waitFor();
}
async function shot(folder, name, source) {
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  });
  if (errors.length || await page.locator('rsbuild-error-overlay').count()) throw new Error(errors.join('\n') || 'Build error overlay');
  await mkdir(`${out}/${folder}`, { recursive: true });
  await page.screenshot({ path: `${out}/${folder}/${name}.png`, animations: 'disabled' });
  captures.push({ folder, name, source, viewport: page.viewportSize() });
  console.log(`${folder}/${name}.png`);
}

try {
  // The final plate inventory defines the primary frames; rendering always comes from the application.
  for (const folder of (await readdir(plates)).filter(folder => /^\d{2}-/.test(folder)).sort()) {
    const surface = folder.slice(3);
    for (const file of (await readdir(`${plates}/${folder}/final`)).filter(name => name.endsWith('.png')).sort()) {
      const stem = file.slice(0, -4), state = stem.slice(0, 2);
      const png = await readFile(`${plates}/${folder}/final/${file}`);
      const params = new URLSearchParams({ surface, data: 'fixture' });
      if (surface === 'brain') {
        params.set('view', ['overview', 'hover', 'repo-zoom', 'scoped', 'synapse'][Number(state) - 1]);
        params.set('dim', '2d');
      }
      if (surface === 'code' && state === '02') { params.set('lens', 'files'); params.set('data', 'snapshot'); params.set('path', 'crates'); }
      if (surface === 'loom' || surface === 'delivery') params.set('state', state);
      if (surface === 'loom') {
        if (state === '08') { params.set('loom_lens', 'proximity'); params.set('state', '04'); }
        params.set('loom_source', 'design');
        if (Number(state) >= 4) params.set('loom_page', 'full');
      }
      // Each primary frame starts without a previous frame's remembered camera or selection.
      await open(params, { width: png.readUInt32BE(16), height: png.readUInt32BE(20) });
      await page.evaluate(() => sessionStorage.clear());
      await page.reload();
      if (surface === 'code' && state === '02') await page.getByRole('button', { name: 'Comparison', exact: true }).click();
      await shot(folder, stem, (['sessions', 'explorer'].includes(surface) || (surface === 'code' && state === '02')) ? 'Recorded snapshot' : 'Authored example');
    }
  }

  await open('surface=brain&data=snapshot');
  const atlas = page.getByRole('region', { name: 'Measured repository atlas' });
  for (const lens of ['Structure', 'Dependencies', 'Changes', 'Cycles', 'Coverage', 'Churn', 'Duplicates', 'Forwarding']) {
    await atlas.getByRole('button', { name: lens, exact: true }).click();
    await atlas.getByRole('button', { name: 'Exact paths', exact: true }).click();
    await atlas.getByRole('textbox', { name: 'Filter exact paths' }).fill(lens === 'Dependencies' ? 'crates/tracedecay-store-runtime' : '');
    await atlas.getByRole('dialog').getByRole('button', { name: lens === 'Dependencies' ? 'crates/tracedecay-store-runtime' : '/', exact: true }).click();
    await atlas.getByRole('button', { name: 'Fit repository', exact: true }).click();
    if (lens === 'Duplicates') {
      const groups = page.getByLabel('Byte-identical file group');
      const value = await groups.locator('option').evaluateAll(options => options.map(option => option.value).find(Boolean));
      if (value) {
        await groups.selectOption(value);
        await atlas.getByRole('button', { name: 'Fit matching files', exact: true }).click();
      }
    }
    if (lens === 'Forwarding') {
      for (const comparison of ['Before', 'After']) {
        await atlas.getByRole('button', { name: new RegExp(`^${comparison} `) }).click();
        await shot('01-brain', `atlas-forwarding-${comparison.toLowerCase()}`, 'Measured Git snapshot');
      }
    } else await shot('01-brain', `atlas-${lens.toLowerCase()}`, 'Measured Git snapshot');
  }

  await open('surface=code&data=fixture&lens=trace');
  await shot('06-code', 'trace', 'Authored example');
  await page.getByRole('tab', { name: 'CORE', exact: true }).click();
  await shot('06-code', 'core-overview', 'Authored example');
  await page.getByLabel('Core semantic zoom').getByRole('button', { name: 'SOURCE RANGE', exact: true }).click();
  await shot('06-code', 'core-source-range', 'Authored example');
  await open('surface=code&data=snapshot&lens=cortex');
  await shot('06-code', 'cortex-recorded', 'Measured Git snapshot');

  await open('surface=loom&data=fixture&loom_source=design&state=04&loom_page=full&loom_lens=proximity&loom_encounter=fixture%3Aproximity%3Agit-conflict');
  await page.getByRole('button', { name: 'REPLAY OBSERVATION', exact: true }).click();
  await page.getByRole('button', { name: 'FOCUS LOCAL INTERVAL', exact: true }).click();
  await shot('03-loom', 'proximity-conflict', 'Authored example');
  await page.getByRole('button', { name: /^Attention: \d+ active sources$/ }).click();
  await page.getByRole('dialog', { name: 'Attention / source evidence' }).locator('.attention-list button').filter({ hasText: 'Fixture · Git conflict between worktrees' }).click();
  await shot('shared', 'source-attention', 'Authored example');
  await open('surface=delivery&data=snapshot&state=03');
  await page.getByLabel('Tracked branch scope unavailable').or(page.getByLabel('Indexed branch relationships')).waitFor();
  await shot('08-delivery', 'indexed-branch-footprint', 'Recorded indexed branch');

  const lines = ['# Current UI screenshots', '', `Captured ${new Date().toISOString().slice(0, 10)} from the concept application in TraceDecay. These are full-resolution desktop screenshots, not design plates.`, '', 'The source column distinguishes authored examples from recorded data. Some example screens also display explicitly labeled snapshot evidence; each screen states its own coverage.', '', 'Regenerate this collection with `BASE_URL=http://127.0.0.1:5195 node scripts/capture-ui.mjs` from `mockups/ui-concept-v2/app`. Use `OUTPUT_DIR` for temporary captures.', '', '| Workspace | View | Source | Resolution |', '| --- | --- | --- | --- |'];
  for (const { folder, name, source, viewport } of captures) lines.push(`| ${title(folder)} | [${title(name)}](${folder}/${name}.png) | ${source} | ${viewport.width}×${viewport.height} |`);
  await writeFile(`${out}/README.md`, `${lines.join('\n')}\n`);
} finally {
  await browser.close();
}
