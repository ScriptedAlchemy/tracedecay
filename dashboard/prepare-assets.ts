import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { constants, gunzipSync, gzipSync } from 'node:zlib';
import type { RsbuildPlugin } from '@rsbuild/core';

const MANIFEST = 'asset-manifest.json';
const COMPRESSIBLE = /\.(html|js|mjs|css|json|map|svg|txt)$/;

function relativePath(value: unknown): string {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    /[\\?#%:]/.test(value) ||
    path.posix.isAbsolute(value) ||
    value.split('/').includes('..')
  ) {
    throw new Error(`dashboard asset manifest contains an unsafe path: ${String(value)}`);
  }
  const normalized = path.posix.normalize(value);
  if (normalized === '.') throw new Error('dashboard asset manifest contains an empty path');
  return normalized;
}

/** Prepare the payloads inside the frontend action, whose inputs Bazel caches.
 * allFiles remains the complete digest/copy inventory; gzip selects an encoded
 * payload for an original route. The Rust consumer only validates and stages it.
 */
export async function prepareDashboardAssets(distPath: string): Promise<void> {
  const manifestPath = path.join(distPath, MANIFEST);
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  if (manifest === null || typeof manifest !== 'object' || !Array.isArray(manifest.allFiles)) {
    throw new Error('dashboard asset manifest must contain an allFiles array');
  }
  const files: string[] = manifest.allFiles.map(relativePath).sort();
  const inventory = new Set(files);
  if (inventory.size !== files.length || !inventory.has('index.html')) {
    throw new Error('dashboard asset manifest must contain unique files and index.html');
  }
  const contents = new Map<string, Buffer>();
  for (const file of files) contents.set(file, await readFile(path.join(distPath, file)));

  // A repeated hook invocation validates the already-prepared inventory. It
  // never treats a payload as another source or overwrites an independent asset.
  if (Object.hasOwn(manifest, 'gzip')) {
    if (manifest.gzip === null || typeof manifest.gzip !== 'object' || Array.isArray(manifest.gzip)) {
      throw new Error('dashboard asset manifest gzip must be an object');
    }
    const mappings = Object.entries(manifest.gzip).map(([source, encoded]) =>
      [relativePath(source), relativePath(encoded)] as const);
    const sources = new Set(mappings.map(([source]) => source));
    if (sources.size !== mappings.length) throw new Error('duplicate dashboard gzip source');
    for (const [source, encoded] of mappings) {
      const raw = contents.get(source);
      const compressed = contents.get(encoded);
      if (encoded !== `${source}.gz` || sources.has(encoded) || !raw || !compressed) {
        throw new Error(`invalid dashboard gzip mapping for ${source}`);
      }
      if (
        compressed.length >= raw.length ||
        !gunzipSync(compressed, { maxOutputLength: raw.length + 1 }).equals(raw)
      ) {
        throw new Error(`dashboard gzip payload does not match ${source}`);
      }
    }
    return;
  }

  const gzip: Record<string, string> = {};
  for (const [file, raw] of contents) {
    if (!COMPRESSIBLE.test(file)) continue;
    const compressed = gzipSync(raw, { level: constants.Z_BEST_COMPRESSION });
    if (!gunzipSync(compressed, { maxOutputLength: raw.length + 1 }).equals(raw)) {
      throw new Error(`dashboard gzip roundtrip did not match ${file}`);
    }
    if (compressed.length >= raw.length) continue;
    const encoded = `${file}.gz`;
    if (inventory.has(encoded)) {
      throw new Error(`dashboard gzip payload would overwrite an asset: ${encoded}`);
    }
    await writeFile(path.join(distPath, encoded), compressed, { flag: 'wx' });
    gzip[file] = encoded;
  }
  manifest.allFiles = [...files, ...Object.values(gzip)].sort();
  manifest.gzip = gzip;
  // Publish the mapping only after every referenced payload has been written.
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
}

export function pluginPreparedDashboardAssets(): RsbuildPlugin {
  return {
    name: 'tracedecay:prepared-dashboard-assets',
    setup(api) {
      api.onAfterBuild(async ({ environments, stats }) => {
        if (stats?.hasErrors()) return;
        const roots = new Set(Object.values(environments).map((environment) => environment.distPath));
        for (const root of roots) await prepareDashboardAssets(root);
      });
    },
  };
}
