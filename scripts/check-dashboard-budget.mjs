#!/usr/bin/env node
/**
 * Enforce plan 11's dashboard payload ceilings on a built bundle.
 *
 * `check-dashboard-bundle.py` next to this file enforces a floor — that the
 * bundle is real and not a placeholder. This enforces the other side: the
 * transfer budgets the plan states, which nothing measured before. When first
 * run, one async chunk was 288 KiB Brotli against the 200 KiB ceiling, because
 * `Chart` imported all of ECharts rather than the two series types the product
 * draws.
 *
 * Brotli via `node:zlib` is the same codec a server negotiates, so these are
 * transfer sizes rather than a proxy for them. No dependency, and it runs in the
 * job that already built the bundle.
 *
 *   node scripts/check-dashboard-budget.mjs [dashboard/app-dist]
 */
import { readFileSync, readdirSync, existsSync, statSync } from 'node:fs';
import path from 'node:path';
import { brotliCompressSync, constants } from 'node:zlib';

const KIB = 1024;

/**
 * Plan 11: "initial shell HTML, critical CSS, and executable JavaScript
 * <=250 KiB Brotli; initial workspace chunk <=200 KiB".
 */
const INITIAL_BUDGET_BYTES = 250 * KIB;
const CHUNK_BUDGET_BYTES = 200 * KIB;

/** Maximum quality, matching what a CDN or `Content-Encoding: br` server sends. */
function brotliBytes(buffer) {
  return brotliCompressSync(buffer, {
    params: { [constants.BROTLI_PARAM_QUALITY]: 11 },
  }).length;
}

function kib(bytes) {
  return `${(bytes / KIB).toFixed(1)} KiB`;
}

/** Local asset paths referenced by `index.html`, in document order. */
function initialReferences(html) {
  const references = [];
  const pattern = /<(script|link)\b[^>]*?\b(?:src|href)="([^"]+)"/gi;
  for (const match of html.matchAll(pattern)) {
    const reference = match[2];
    if (/^[a-z]+:|^\/\//i.test(reference)) continue;
    references.push(reference.replace(/^\//, ''));
  }
  return references;
}

function measure(bundle) {
  const index = path.join(bundle, 'index.html');
  if (!existsSync(index)) {
    throw new Error(`dashboard budget: index.html is missing: ${index}`);
  }
  const html = readFileSync(index, 'utf8');

  const initial = [{ label: 'index.html', bytes: brotliBytes(Buffer.from(html)) }];
  for (const reference of initialReferences(html)) {
    const asset = path.join(bundle, reference);
    if (!existsSync(asset)) {
      throw new Error(`dashboard budget: referenced asset is missing: ${reference}`);
    }
    initial.push({ label: reference, bytes: brotliBytes(readFileSync(asset)) });
  }
  // A bundle whose index loads no local JavaScript would otherwise pass every
  // ceiling below by measuring almost nothing.
  if (initial.length < 2) {
    throw new Error('dashboard budget: index.html references no local assets');
  }

  const asyncDir = path.join(bundle, 'static/js/async');
  const chunks = existsSync(asyncDir)
    ? readdirSync(asyncDir)
        .filter((name) => name.endsWith('.js'))
        .map((name) => ({
          label: `static/js/async/${name}`,
          bytes: brotliBytes(readFileSync(path.join(asyncDir, name))),
        }))
        .sort((a, b) => b.bytes - a.bytes)
    : [];
  // Every workspace is a lazy route, so an absent async directory means the
  // code-split contract broke rather than that there is nothing to check.
  if (chunks.length === 0) {
    throw new Error(
      `dashboard budget: no lazy route chunks found under ${asyncDir}; ` +
        'the twelve workspaces are code-split, so this bundle is not the product',
    );
  }

  return { initial, chunks };
}

function main() {
  const bundle = process.argv[2] ?? path.join('dashboard', 'app-dist');
  if (!existsSync(bundle) || !statSync(bundle).isDirectory()) {
    console.error(`dashboard budget: bundle directory is missing: ${bundle}`);
    process.exit(1);
  }

  let measured;
  try {
    measured = measure(bundle);
  } catch (error) {
    console.error(String(error.message ?? error));
    process.exit(1);
  }

  const initialBytes = measured.initial.reduce((total, item) => total + item.bytes, 0);
  const breaches = [];

  console.log(`dashboard payload budgets (Brotli, from ${bundle})`);
  console.log(`  initial payload  ${kib(initialBytes)} / ${kib(INITIAL_BUDGET_BYTES)}`);
  for (const item of measured.initial) {
    console.log(`      ${item.label.padEnd(44)} ${kib(item.bytes)}`);
  }
  if (initialBytes > INITIAL_BUDGET_BYTES) {
    breaches.push(
      `initial payload is ${kib(initialBytes)}, over the ${kib(INITIAL_BUDGET_BYTES)} ceiling`,
    );
  }

  console.log(`  lazy chunks      ${measured.chunks.length} found, each / ${kib(CHUNK_BUDGET_BYTES)}`);
  for (const chunk of measured.chunks.slice(0, 5)) {
    console.log(`      ${chunk.label.padEnd(44)} ${kib(chunk.bytes)}`);
  }
  for (const chunk of measured.chunks) {
    if (chunk.bytes > CHUNK_BUDGET_BYTES) {
      breaches.push(
        `${chunk.label} is ${kib(chunk.bytes)}, over the ${kib(CHUNK_BUDGET_BYTES)} ceiling`,
      );
    }
  }

  if (breaches.length > 0) {
    console.error('\ndashboard budget: over plan 11 ceilings');
    for (const breach of breaches) console.error(`  - ${breach}`);
    console.error(
      '\nA chunk usually grows because a library was imported whole. Import the\n' +
        'modules actually used (see dashboard/src/viz/chart/echarts.ts) rather than\n' +
        'raising a ceiling the plan states.',
    );
    process.exit(1);
  }

  console.log('\ndashboard budget: within plan 11 ceilings');
}

main();
