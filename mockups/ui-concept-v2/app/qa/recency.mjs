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
const normal = layout(fixture.PROJECTS), reversed = layout([...fixture.PROJECTS].reverse());
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
    await page.getByRole('tab',{name:'Hover',exact:true}).click();
    await page.getByRole('button',{name:/^Inspect core, indexed mass/}).focus();
    await page.getByRole('tab',{name:'Overview',exact:true}).click();
    assert.deepEqual(await page.locator('.project-label:not(.repo-label)').evaluateAll(nodes=>nodes.map(node=>({name:node.getAttribute('aria-label'),x:parseFloat(node.style.left)}))),actual,'Inspection cannot move the registry centers');
    if(process.env.OUTPUT_DIR){mkdirSync(process.env.OUTPUT_DIR,{recursive:true});await page.screenshot({path:join(process.env.OUTPUT_DIR,`recency-aligned-${viewport.width}.png`)});}
  }
  assert.deepEqual(errors,[]);
  console.log('PASS printed recency bounds at1586/793, real DOM/layout agreement, stable production registry ordering and inspection positions.');
} finally { await browser.close(); }
