// BASE_URL=http://127.0.0.1:5195 OUTPUT_DIR=/optional/evidence node qa/recency.mjs
import assert from 'node:assert/strict';
import { readFileSync, existsSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import ts from 'typescript';
import { chromium } from 'playwright';
function compile(path, dependencies = {}) {
  const source = readFileSync(new URL(path, import.meta.url), 'utf8');
  const module = {exports:{}};
  new Function('require','exports','module',ts.transpileModule(source,{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText)(id => {assert.ok(id in dependencies,`unexpected dependency ${id}`);return dependencies[id];},module.exports,module);
  return module.exports;
}
const fixture = compile('../src/data/fixtures.ts',{'./core-branches.json':{default:JSON.parse(readFileSync(new URL('../src/data/core-branches.json',import.meta.url),'utf8'))}});
const particles = compile('../src/brain/particles.ts');
const offsets = compile('../src/brain/recencyLayout.ts');
const layout = projects => compile('../src/brain/layout.ts',{'../data/fixtures':{...fixture,PROJECTS:projects},'./particles':particles,'./recencyLayout':offsets});
const repoLayout = compile('../src/brain/repoLayout.ts');
const normal = layout(fixture.PROJECTS), reversed = layout([...fixture.PROJECTS].reverse());
const circleHitsBox = (circle, box) => {
  const x = Math.max(box.x, Math.min(circle.x, box.x + box.width));
  const y = Math.max(box.y, Math.min(circle.y, box.y + box.height));
  return Math.hypot(circle.x - x, circle.y - y) < circle.r;
};
function assertNoBodyOrLabelCollisions(bodies, labels, prefix) {
  for (let i = 0; i < bodies.length; i++) {
    for (let j = i + 1; j < bodies.length; j++) {
      const a = bodies[i], b = bodies[j];
      assert.ok(Math.hypot(a.x - b.x, a.y - b.y) >= a.r + b.r, `${prefix}: ${a.id} intersects ${b.id} (${JSON.stringify(a)} / ${JSON.stringify(b)})`);
    }
  }
  for (const label of labels) {
    for (const body of bodies) {
      if (label.id !== body.id) assert.ok(!circleHitsBox(body, label), `${prefix}: ${label.id} label intersects ${body.id} ring`);
    }
  }
}
const executablePath = [process.env.CHROMIUM_PATH,chromium.executablePath(),'/usr/bin/chromium','/usr/bin/google-chrome'].find(path=>path&&existsSync(path));
const browser = await chromium.launch({...(executablePath?{executablePath}:{}),args:['--no-sandbox']});
try {
  const page = await browser.newPage({reducedMotion:'reduce'});
  const errors=[];page.on('pageerror',error=>errors.push(error.message));
  for(const viewport of [{width:1586,height:992},{width:793,height:700}]) {
    await page.setViewportSize(viewport);
    const url=new URL(process.env.BASE_URL??'http://127.0.0.1:5195');
    url.searchParams.set('surface','brain');url.searchParams.set('data','fixture');url.searchParams.set('view','overview');url.searchParams.set('dim','2d');
    await page.goto(url.href,{waitUntil:'networkidle'});
    const aperture=await page.locator('.aperture').boundingBox();
    assert.match(await page.locator('.brain-field-key').innerText(), /REGISTERED INDEXED PROJECTS.*BODY AREA.*INDEXED MASS.*HORIZONTAL POSITION.*RECENCY/is, 'registry field must name its admission boundary and the two visual encodings');
    const actual=await page.locator('.project-label:not(.repo-label)').evaluateAll(nodes=>nodes.map(node=>({name:node.getAttribute('aria-label'),x:parseFloat(node.style.left)})));
    const ticks=await page.locator('.axis-x .tick').evaluateAll(nodes=>nodes.map(node=>({x:node.getBoundingClientRect().x,width:node.getBoundingClientRect().width})));
    const field=normal.layoutField(aperture.width,aperture.height), reordered=reversed.layoutField(aperture.width,aperture.height);
    assert.equal(actual.length,fixture.PROJECTS.length);
    for(const body of field.bodies) {
      const node=actual.find(node=>node.name.startsWith(`Inspect ${body.project.name},`));
      assert.ok(node,body.project.name);
      assert.ok(Math.abs(node.x-body.x)<.01,'DOM picking caption must share the rendered layout center');
      assert.equal(reordered.bodies.find(candidate=>candidate.project.id===body.project.id).x,body.x,'Production layout centers must survive reordered registry input');
      const index=fixture.RECENCY_AXIS.findIndex(bucket=>bucket.id===body.project.recency),tick=ticks[index];
      assert.ok(node.x+aperture.x>tick.x && node.x+aperture.x<tick.x+tick.width,`${viewport.width}px: ${body.project.name} center ${node.x} outside printed ${body.project.recency} band ${tick.x-aperture.x}–${tick.x+tick.width-aperture.x}`);
    }
    assertNoBodyOrLabelCollisions(
      field.bodies.map(body => ({id:body.project.id,x:body.x,y:body.y,r:body.capR})),
      viewport.width < 1000 ? [] : (await page.locator('.project-label:not(.repo-label)').evaluateAll(nodes => nodes.map(node => {
        const box=node.getBoundingClientRect();
        return {id:node.getAttribute('aria-label').match(/^Inspect (.*), indexed mass/)?.[1],x:box.x,y:box.y,width:box.width,height:box.height};
      }))).map(label => ({...label,id:field.bodies.find(body=>body.project.name===label.id)?.project.id,x:label.x-aperture.x,y:label.y-aperture.y})),
      `fixture overview ${viewport.width}px`,
    );
    await page.getByRole('tab',{name:'Hover',exact:true}).click();
    await page.getByRole('button',{name:/^Inspect core, indexed mass/}).focus();
    await page.getByRole('tab',{name:'Overview',exact:true}).click();
    assert.deepEqual(await page.locator('.project-label:not(.repo-label)').evaluateAll(nodes=>nodes.map(node=>({name:node.getAttribute('aria-label'),x:parseFloat(node.style.left)}))),actual,'Inspection cannot move the registry centers');
    if(process.env.OUTPUT_DIR){mkdirSync(process.env.OUTPUT_DIR,{recursive:true});await page.screenshot({path:join(process.env.OUTPUT_DIR,`recency-aligned-${viewport.width}.png`)});}
  }
  await page.setViewportSize({width:1586,height:992});
  const fixtureUrl=new URL(process.env.BASE_URL??'http://127.0.0.1:5195');
  fixtureUrl.searchParams.set('surface','brain');fixtureUrl.searchParams.set('data','fixture');fixtureUrl.searchParams.set('view','repo-zoom');fixtureUrl.searchParams.set('dim','2d');
  await page.goto(fixtureUrl.href,{waitUntil:'networkidle'});
  const repoAperture=await page.locator('.aperture').boundingBox();
  const repo=repoLayout.layoutRepoField(repoAperture.width,repoAperture.height,fixture.PROJECTS[0],repoLayout.REPO_DESIGN_ZOOM);
  assertNoBodyOrLabelCollisions(
    repo.orbs.map(orb=>({id:orb.id,x:orb.x,y:orb.y,r:orb.radius})),
    (await page.locator('.project-label.repo-label').evaluateAll(nodes=>nodes.map(node=>{
      const box=node.getBoundingClientRect();
      return {id:node.getAttribute('aria-label').replace('Inspect checkout ',''),x:box.x,y:box.y,width:box.width,height:box.height};
    }))).map(label=>({...label,x:label.x-repoAperture.x,y:label.y-repoAperture.y})),
    'fixture repository zoom',
  );
  const snapshotUrl=new URL(process.env.BASE_URL??'http://127.0.0.1:5195');
  snapshotUrl.searchParams.set('surface','brain');snapshotUrl.searchParams.set('data','snapshot');
  await page.goto(snapshotUrl.href,{waitUntil:'networkidle'});
  const snapshotField=await page.locator('.snapshot-brain-field').boundingBox();
  const snapshotBodies=(await page.locator('.snapshot-project').evaluateAll(nodes=>nodes.map(node=>{
    const ring=node.querySelector('i').getBoundingClientRect(), label=node.querySelector('b').getBoundingClientRect();
    return {id:node.textContent.trim(),ring:{x:ring.x+ring.width/2,y:ring.y+ring.height/2,r:ring.width/2},label:{x:label.x,y:label.y,width:label.width,height:label.height}};
  }))).map(body=>({...body,ring:{...body.ring,x:body.ring.x-snapshotField.x,y:body.ring.y-snapshotField.y},label:{...body.label,x:body.label.x-snapshotField.x,y:body.label.y-snapshotField.y}}));
  assertNoBodyOrLabelCollisions(
    snapshotBodies.map(body=>({id:body.id,...body.ring})),
    snapshotBodies.map(body=>({id:body.id,...body.label})),
    'snapshot overview',
  );
  assert.deepEqual(errors,[]);
  console.log('PASS printed recency bounds, deterministic non-intersecting fixture/snapshot bodies and labels, and fixture repository zoom.');
} finally { await browser.close(); }
