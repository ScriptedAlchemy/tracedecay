import assert from 'node:assert/strict';
import { chromium } from 'playwright';

const browser = await chromium.launch();
try {
  // A 1280×852 window at 200% zoom has a 640×426 CSS viewport.
  const page = await browser.newPage({viewport:{width:640,height:426},reducedMotion:'reduce'});
  const base=process.env.BASE_URL ?? 'http://127.0.0.1:5195';
  const errors=[]; page.on('pageerror',error=>errors.push(error.message));
  for (const state of ['01','02','03','04','05','06','07']) {
    await page.goto(`${base}/?surface=loom&loom_source=design&state=${state}`);
    await page.locator('.journey-field canvas').waitFor();
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),`${state}: no horizontal document overflow`);
    await page.getByRole('combobox',{name:'Loom data source',exact:true}).click({trial:true});
    await page.getByRole('button',{name:'Next event',exact:true}).click({trial:true});
    if (state==='03') await page.getByRole('button',{name:'COLLAPSE CHILDREN',exact:true}).click({trial:true});
    if (state==='04') {
      await page.getByRole('searchbox',{name:'Search branches',exact:true}).fill('Dashboard');
      await page.getByRole('searchbox',{name:'Search branches',exact:true}).fill('');
    }
    if (state==='05') await page.getByRole('button',{name:'FOCUS EXACT SOURCE',exact:true}).click({trial:true});
    if (state==='06') await page.getByLabel('Comment / evidence rationale').fill('Keyboard-accessible local draft');
    if (state==='07') {
      await page.locator('.loom-adjudication-form summary').click();
      await page.getByLabel('Resolution rationale').fill('Keyboard-accessible assessment draft');
    }
    await page.locator('.journey-fallbacks summary').first().click();
    await page.locator('.journey-fallbacks summary').first().focus();
    assert.ok(await page.locator('.journey-fallbacks summary').first().evaluate(el=>el===document.activeElement));
  }
  assert.deepEqual(errors,[]);
  console.log('PASS: seven Loom views reflow at 200%, with reachable source, transport, branch, evidence, feedback, adjudication and exact-table controls.');
} finally {await browser.close();}
