import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { gunzipSync } from 'node:zlib';
import { afterEach, expect, test } from 'vitest';
import { prepareDashboardAssets } from '../../prepare-assets.ts';

const roots: string[] = [];
const javascript = Buffer.from('console.log("dashboard payload");\n'.repeat(1000));

afterEach(async () => {
  await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

async function fixture(extra: Record<string, Buffer> = {}) {
  const root = await mkdtemp(path.join(tmpdir(), 'dashboard-assets-'));
  roots.push(root);
  const files = {
    'index.html': Buffer.from('<script src="/static/app.js"></script>'),
    'static/app.js': javascript,
    'tiny.txt': Buffer.from('tiny'),
    'font.woff2': Buffer.from('already encoded font data'.repeat(100)),
    ...extra,
  };
  for (const [relative, bytes] of Object.entries(files)) {
    await mkdir(path.dirname(path.join(root, relative)), { recursive: true });
    await writeFile(path.join(root, relative), bytes);
  }
  await writeFile(path.join(root, 'asset-manifest.json'), JSON.stringify({ allFiles: Object.keys(files) }));
  return root;
}

async function manifest(root: string) {
  return JSON.parse(await readFile(path.join(root, 'asset-manifest.json'), 'utf8'));
}

test('the producer inventories selected gzip payloads and repeated preparation preserves bytes', async () => {
  const root = await fixture();
  await prepareDashboardAssets(root);
  const first = await manifest(root);
  expect(first.gzip).toEqual({ 'static/app.js': 'static/app.js.gz' });
  expect(first.allFiles).toContain('static/app.js.gz');
  const compressed = await readFile(path.join(root, 'static/app.js.gz'));
  expect(compressed.length).toBeLessThan(javascript.length);
  expect(gunzipSync(compressed)).toEqual(javascript);
  await prepareDashboardAssets(root);
  expect(await manifest(root)).toEqual(first);
  expect(await readFile(path.join(root, 'static/app.js.gz'))).toEqual(compressed);
  const secondRoot = await fixture();
  await prepareDashboardAssets(secondRoot);
  expect(await manifest(secondRoot)).toEqual(first);
  expect(await readFile(path.join(secondRoot, 'static/app.js.gz'))).toEqual(compressed);
});

test.each([true, false])('an independent gzip path is not overwritten (inventoried: %s)', async (inventoried) => {
  const independent = Buffer.from('independent compressed download');
  const root = await fixture(inventoried ? { 'static/app.js.gz': independent } : {});
  if (!inventoried) await writeFile(path.join(root, 'static/app.js.gz'), independent);
  await expect(prepareDashboardAssets(root)).rejects.toThrow();
  expect(await readFile(path.join(root, 'static/app.js.gz'))).toEqual(independent);
});

test('already compressed independent assets remain identity files', async () => {
  const root = await fixture({ 'archive.gz': Buffer.from('archive bytes') });
  await prepareDashboardAssets(root);
  const prepared = await manifest(root);
  expect(prepared.allFiles).toContain('archive.gz');
  expect(prepared.allFiles).not.toContain('archive.gz.gz');
  expect(prepared.gzip['archive.gz']).toBeUndefined();
});

test('a prepared inventory fails closed after payload or mapping corruption', async () => {
  const root = await fixture();
  await prepareDashboardAssets(root);
  const prepared = await manifest(root);
  prepared.gzip['static/app.js'] = '../outside.gz';
  await writeFile(path.join(root, 'asset-manifest.json'), JSON.stringify(prepared));
  await expect(prepareDashboardAssets(root)).rejects.toThrow('unsafe path');
  prepared.gzip['static/app.js'] = 'static/app.js.gz';
  await writeFile(path.join(root, 'asset-manifest.json'), JSON.stringify(prepared));
  await writeFile(path.join(root, 'static/app.js'), Buffer.from('different identity bytes'));
  await expect(prepareDashboardAssets(root)).rejects.toThrow();
});
