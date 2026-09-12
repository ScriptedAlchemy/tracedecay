// Focused measured-atlas regression: npm install, start dev server, then
// BASE_URL=http://127.0.0.1:5195 node qa/structure.mjs
import { chromium } from 'playwright';
import { existsSync, readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import assert from 'node:assert/strict';
const data = JSON.parse(readFileSync(new URL('../src/structure/snapshot.json', import.meta.url), 'utf8'));
const ids = new Set(data.nodes.map(node => node.id));
assert.ok(data.edges.every(edge => ids.has(edge.source) && ids.has(edge.target)));
assert.equal(data.churn.pathTouches, data.churn.files.reduce((sum, file) => sum + file.touches, 0));
assert.equal(Date.parse(data.churn.end) - Date.parse(data.churn.start), data.churn.days * 86400000);
assert.ok(data.churn.files.every(file => ids.has(file.path) && file.touches > 0 && file.lastTouchedAt * 1000 >= Date.parse(data.churn.start) && file.lastTouchedAt * 1000 <= Date.parse(data.churn.end)));
assert.ok(data.duplicateFiles.groups.every(group => group.bytes > 0 && group.paths.length > 1 && new Set(group.paths).size === group.paths.length && group.paths.every(path => ids.has(path))));
const executablePath = [process.env.CHROMIUM_PATH, chromium.executablePath(), '/usr/bin/chromium', '/usr/local/bin/google-chrome', '/usr/bin/google-chrome', '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'].find(path => path && existsSync(path));
const browser = await chromium.launch({ ...(executablePath ? { executablePath } : {}), args: ['--no-sandbox'] });
try {
  const page = await browser.newPage({ viewport: { width: 1586, height: 992 }, reducedMotion: 'reduce' });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  if (!process.env.INSPECTOR_ONLY) {
  if (!process.env.EXPLORER_ONLY) {
  await page.addInitScript(() => { if (!sessionStorage.getItem('td:view:snapshot:repository-atlas')) sessionStorage.setItem('td:view:snapshot:repository-atlas', '{"selected":"","camera":{"x":1e309,"y":"bad","scale":-1},"layer":"invalid","comparison":"invalid"}'); });
  const url = new URL(process.env.BASE_URL ?? 'http://127.0.0.1:5195');
  url.searchParams.set('surface', 'brain'); url.searchParams.set('data', 'snapshot');
  await page.goto(url.href, { waitUntil: 'networkidle' });
  // Accepted Brain composition is registry-first. The measured atlas remains a
  // separately labelled repository-structure option, rather than the default
  // knowledge geometry for every recorded snapshot.
  assert.match(await page.getByLabel('Recorded Brain snapshot').innerText(), /REGISTRY · READY[\s\S]*ACTIVITY · EMPTY/);
  await page.getByRole('button', { name: 'Atlas / repository structure', exact: true }).click();
  const state = () => page.evaluate(() => JSON.parse(sessionStorage.getItem('td:view:snapshot:repository-atlas')));
  assert.equal((await state()).camera, null);
  assert.equal((await state()).layer, 'structure');
  assert.equal((await state()).comparison, 'after');
  const atlas = page.getByRole('region', { name: 'Measured repository atlas' });
  const map = atlas.locator('.atlas-stage > canvas').first();
  await atlas.getByRole('button', { name: 'Zoom in', exact: true }).click();
  const camera = (await state()).camera;
  assert.ok(Number.isFinite(camera.x) && Number.isFinite(camera.y) && camera.scale > 0);
  const structurePixels = await map.screenshot();
  await atlas.getByRole('button', { name: 'Churn', exact: true }).click();
  assert.deepEqual((await state()).camera, camera);
  assert.match(await atlas.locator('.atlas-inspect').innerText(), new RegExp(`${data.churn.pathTouches} commit/file touches`));
  assert.match(await atlas.locator('.atlas-inspect').innerText(), /not line counts or defects/);
  if (data.churn.pathTouches) assert.notDeepEqual(await map.screenshot(), structurePixels);
  await atlas.getByRole('button', { name: 'Duplicates', exact: true }).click();
  assert.deepEqual((await state()).camera, camera);
  const group = data.duplicateFiles.groups[0];
  if (group) {
    await atlas.getByRole('combobox', { name: 'Byte-identical file group' }).selectOption(group.blob);
    assert.equal((await state()).selected, group.paths[0]);
    await atlas.getByRole('button', { name: 'Fit matching files', exact: true }).click();
    const second = group.paths[1];
    await atlas.getByRole('button', { name: `Inspect matching path · ${second}`, exact: true }).click();
    assert.equal(await atlas.locator('.atlas-inspect h3').innerText(), second);
    assert.ok((await atlas.getByRole('link', { name: 'Pinned matching source ↗' }).all()).length === group.paths.length);
  } else assert.match(await atlas.locator('.atlas-inspect').innerText(), /No nonempty byte-identical source files/);
  const output = process.env.OUTPUT_DIR;
  const captures = [];
  if (output) mkdirSync(output, {recursive:true});
  async function capture(name) {
    await page.evaluate(() => document.fonts.ready);
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    if (output) { await page.screenshot({path:join(output, `${name}.png`)}); captures.push(name); }
  }
  async function choosePath(path) {
    await atlas.getByRole('button',{name:'Exact paths',exact:true}).click();
    const dialog = atlas.getByRole('dialog',{name:'Exact tracked paths'});
    await dialog.getByRole('textbox',{name:'Filter exact paths'}).fill(path);
    await dialog.getByRole('button',{name:path || '/',exact:true}).click();
    assert.equal((await state()).selected,path);
  }
  const ghost = data.changes.find(change=>change.status==='removed').path;
  await atlas.getByRole('button',{name:'Structure',exact:true}).click();
  await choosePath(ghost);
  // A fit must make even a tiny removed cell selectable, with explicit baseline identity.
  let box = await map.boundingBox();
  await map.click({position:{x:(box.width-270)/2+14,y:box.height/2}});
  assert.equal((await state()).selected,ghost);
  assert.match(await atlas.locator('.atlas-inspect').innerText(),/baseline ghost/);
  await capture('desktop-selected-baseline-ghost');
  await choosePath('Cargo.toml');
  await atlas.getByRole('button',{name:'Changes',exact:true}).click();
  await atlas.getByRole('button',{name:`Before ${data.baselineRevision.slice(0,8)}`,exact:true}).click();
  // Unrelated paths retain their measured-head source authority in a focused diff.
  assert.match(await atlas.getByRole('link',{name:/Open source/}).getAttribute('href'),new RegExp(data.revision));
  for (const viewport of [{width:1586,height:992,name:'desktop'},{width:793,height:700,name:'narrow'}]) {
    await page.setViewportSize(viewport);
    for (const lens of ['Structure','Dependencies','Changes','Cycles','Forwarding','Coverage','Churn','Duplicates']) {
      await atlas.getByRole('button',{name:lens,exact:true}).click();
      await choosePath(lens==='Dependencies' ? 'crates/tracedecay-store-runtime' : lens==='Changes' ? 'crates/tracedecay-application/src/tracedecay' : '');
      if (lens==='Dependencies') await atlas.getByRole('button',{name:'Fit repository',exact:true}).click();
      const priorCamera = (await state()).camera;
      if (lens==='Changes' || lens==='Forwarding') {
        let beforePixels;
        for (const comparison of ['Before','After']) {
          await atlas.getByRole('button',{name:new RegExp(`^${comparison} `)}).click();
          assert.deepEqual((await state()).camera,priorCamera);
          await capture(`${viewport.name}-${lens.toLowerCase()}-${comparison.toLowerCase()}`);
          if (lens==='Changes') { const pixels=await map.screenshot(); if(comparison==='Before') beforePixels=pixels; else assert.notDeepEqual(pixels,beforePixels,'Removed file geometry must differ before/after even at narrow width'); }
        }
      } else {
        if (lens==='Duplicates' && group) {
          await atlas.getByRole('combobox',{name:'Byte-identical file group'}).selectOption(group.blob);
          await atlas.getByRole('button',{name:'Fit matching files',exact:true}).click();
        }
        await capture(`${viewport.name}-${lens.toLowerCase()}`);
      }
      const atlasBox = await atlas.boundingBox();
      assert.ok(atlasBox.x>=0 && atlasBox.x+atlasBox.width<=viewport.width+1);
      assert.ok(atlasBox.y+atlasBox.height<=viewport.height+1);
    }
    await atlas.getByRole('button',{name:'Structure',exact:true}).click();
    await choosePath('crates/tracedecay-store-runtime/src/lib.rs');
    await map.focus();
    const fitted = (await state()).camera;
    await map.press('ArrowRight'); assert.equal((await state()).camera.x,fitted.x-45);
    await map.press('+'); assert.ok((await state()).camera.scale>fitted.scale);
    await map.press('Home');
    box = await map.boundingBox();
    let prior = (await state()).camera;
    await page.mouse.move(box.x+box.width/2,box.y+box.height/2);
    await page.mouse.down();await page.mouse.move(box.x+box.width/2+30,box.y+box.height/2+20);await page.mouse.up();
    assert.equal((await state()).camera.x,prior.x+30);
    prior = (await state()).camera;
    await atlas.locator('.atlas-minimap').click({position:{x:30,y:30}});
    assert.notEqual((await state()).camera.x,prior.x);
    prior = (await state()).camera;
    await atlas.getByRole('button',{name:'Open same place in Code',exact:true}).click();
    await page.getByRole('tab',{name:'EXACT FILES',exact:true}).waitFor();
    assert.equal(new URL(page.url()).searchParams.get('surface'),'code');
    assert.equal((await state()).selected,'crates/tracedecay-store-runtime/src/lib.rs');
    assert.deepEqual((await state()).camera,prior);
    await capture(`${viewport.name}-code-exact-files`);
    const codeAtlasBox = await atlas.boundingBox();
    assert.ok(codeAtlasBox.y+codeAtlasBox.height<=viewport.height+1, 'Code atlas controls must fit the viewport without auto-scroll');
    await atlas.getByRole('button',{name:'Open same place in Brain',exact:true}).click();
    assert.equal(new URL(page.url()).searchParams.get('surface'),'brain');
    assert.equal((await state()).selected,'crates/tracedecay-store-runtime/src/lib.rs');
    assert.deepEqual((await state()).camera,prior);
    await capture(`${viewport.name}-brain-return`);
  }
  if (output) writeFileSync(join(output,'gallery.html'), `<!doctype html><meta charset="utf-8"><title>Structural atlas review</title><style>body{background:#061019;color:#bde3ee;font:16px system-ui;margin:24px}nav{display:flex;flex-wrap:wrap;gap:12px}a{color:#64eeff}img{max-width:100%;height:auto;border:1px solid #315868}figure{margin:30px 0}figcaption{margin:8px 0}</style><h1>Structural atlas · pinned Git ${data.revision.slice(0,8)}</h1><p>All eight lenses, focused before/after, exact paths and Brain ↔ Code camera continuity. Desktop 1586 × 992; narrow 793 × 700. Images retain native resolution.</p><nav>${captures.map(name=>`<a href="#${name}">${name}</a>`).join('')}</nav>${captures.map(name=>`<figure id="${name}"><figcaption>${name}</figcaption><a href="${name}.png"><img src="${name}.png" alt="${name}" loading="lazy"></a></figure>`).join('')}`);
  assert.deepEqual(errors, []);
  console.log(`PASS all eight atlas lenses at 1586×992 and 793×700, focused before/after, baseline picking, path selection, pan/zoom/minimap, Brain↔Code camera continuity; ${data.churn.pathTouches} measured touches and ${data.duplicateFiles.groups.length} exact-blob groups.`);
  }
  await page.setViewportSize({width:1586,height:992});
  const explorerUrl = new URL(process.env.BASE_URL ?? 'http://127.0.0.1:5195');
  explorerUrl.searchParams.set('surface','explorer');explorerUrl.searchParams.set('data','snapshot');
  await page.goto(explorerUrl.href,{waitUntil:'networkidle'});
  const firstIds = await page.locator('.sess-card .row-sub').allTextContents();
  await page.getByRole('button',{name:'Next sessions',exact:true}).click();
  assert.notDeepEqual(await page.locator('.sess-card .row-sub').allTextContents(),firstIds);
  assert.match(await page.locator('.lane-sess .ex-foot').innerText(),/6–10 of/);
  await page.getByRole('button',{name:'Previous sessions',exact:true}).click();
  assert.deepEqual(await page.locator('.sess-card .row-sub').allTextContents(),firstIds);
  assert.match(await page.locator('.lane-know .ex-foot').innerText(),/not served/);
  await page.getByRole('button',{name:'Fit map',exact:true}).click();
  const canvasBounds = await page.locator('.repository-atlas .atlas-stage > canvas').first().boundingBox();
  const compactCamera = await page.evaluate(() => JSON.parse(sessionStorage.getItem('td:view:snapshot:repository-atlas')).camera);
  assert.ok(compactCamera.scale * 1600 > canvasBounds.width * .65, 'Compact atlas must use its available lane width');
  if (process.env.OUTPUT_DIR) { mkdirSync(process.env.OUTPUT_DIR,{recursive:true}); await page.screenshot({path:join(process.env.OUTPUT_DIR,'explorer-desktop.png')}); }
  await page.setViewportSize({width:793,height:700});
  await page.getByRole('combobox',{name:'LANES filter'}).selectOption('sessions');
  await page.getByRole('button',{name:'Next sessions',exact:true}).click();
  const selectedLabel = await page.locator('.sess-card.is-on .row-sub').innerText();
  if (process.env.OUTPUT_DIR) await page.screenshot({path:join(process.env.OUTPUT_DIR,'explorer-narrow-page2.png')});
  await page.getByRole('button',{name:'Open session ↗',exact:true}).click();
  await page.waitForURL(url=>url.searchParams.get('surface')==='sessions');
  const exactId = new URL(page.url()).searchParams.get('session');
  assert.ok(exactId && selectedLabel.includes(exactId.slice(0,10)));
  assert.ok(!(await page.locator('body').innerText()).includes(`Session ${exactId} is unavailable`));
  assert.deepEqual(errors,[]);
  console.log('PASS Explorer paging, exact session navigation, typed Knowledge absence, compact atlas fit, and narrow session controls.');
  }
  for (const viewport of [{width:1263,height:931},{width:793,height:700},{width:1586,height:992}]) {
    await page.setViewportSize(viewport);
    const url=new URL(process.env.BASE_URL ?? 'http://127.0.0.1:5195');
    url.searchParams.set('surface','brain');url.searchParams.set('data','snapshot');url.searchParams.set('atlas','1');
    await page.goto(url.href,{waitUntil:'networkidle'});
    const atlas=page.getByRole('region',{name:'Measured repository atlas'});
    await atlas.getByRole('button',{name:'Dependencies',exact:true}).click();
    await atlas.getByRole('button',{name:'Exact paths',exact:true}).click();
    await atlas.getByRole('textbox',{name:'Filter exact paths'}).fill('crates/tracedecay-store-runtime');
    await atlas.getByRole('dialog').getByRole('button',{name:'crates/tracedecay-store-runtime',exact:true}).click();
    const inspector=atlas.locator('.atlas-inspect');
    const target=inspector.getByRole('button',{name:/→ graph-db\s+dependencies/});
    await target.scrollIntoViewIfNeeded();
    assert.ok(await inspector.evaluate(el=>el.scrollTop)>0,'Reproduction requires a scrolled inspector');
    await target.click();
    await inspector.getByRole('heading',{name:'crates/tracedecay-graph-db',exact:true}).waitFor();
    await page.waitForFunction(()=>document.querySelector('.atlas-inspect').scrollTop===0);
    await atlas.getByRole('button',{name:'Fit repository',exact:true}).click();
    const heading=await inspector.locator('h3').boundingBox(),panel=await inspector.boundingBox();
    assert.ok(heading.y>=panel.y && heading.y+heading.height<=panel.y+panel.height,'New selection heading must actually be inside inspector viewport');
    assert.equal(await inspector.evaluate(el=>el.scrollTop),0);
    const row=inspector.locator('.atlas-dependency-row').first();
    const button=await row.locator('button').boundingBox(),link=await row.locator('a').boundingBox();
    assert.ok(link.y>=button.y+button.height,'Source link must occupy a separate line');
    assert.ok(link.x+link.width<=panel.x+panel.width && button.x+button.width<=panel.x+panel.width);
    if(process.env.OUTPUT_DIR) {
      mkdirSync(process.env.OUTPUT_DIR,{recursive:true});
      await page.screenshot({path:join(process.env.OUTPUT_DIR,`inspector-after-${viewport.width}.png`)});
    }
    // Panning/zooming the same object must not disturb the user's inspector position.
    await row.locator('button').scrollIntoViewIfNeeded();
    const offset=await inspector.evaluate(el=>el.scrollTop);
    const camera=await page.evaluate(()=>JSON.parse(sessionStorage.getItem('td:view:snapshot:repository-atlas')).camera);
    await atlas.getByRole('button',{name:'Zoom in',exact:true}).click();
    assert.equal(await inspector.evaluate(el=>el.scrollTop),offset);
    const next=await page.evaluate(()=>JSON.parse(sessionStorage.getItem('td:view:snapshot:repository-atlas')).camera);
    assert.ok(next.scale>camera.scale);
  }
  assert.deepEqual(errors,[]);
  console.log('PASS inspector identity-change scroll reset, visible headings, separate dependency sources and preserved camera-control scroll at1263,793,1586px.');
} finally { await browser.close(); }
