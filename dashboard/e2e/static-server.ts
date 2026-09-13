/**
 * Static bundle serving and animation quiescing for the visual audit.
 *
 * Split out of the removed accessibility harness. Nothing here is
 * accessibility-specific: it builds the dashboard bundle into a scratch
 * directory, serves it, and supplies an init script that freezes animation so
 * screenshots are deterministic.
 */
import { spawnSync } from 'node:child_process';
import { cpSync, createReadStream, existsSync, mkdirSync, readFileSync, rmSync, statSync } from 'node:fs';
import { createServer, type Server } from 'node:http';
import path from 'node:path';

const ROOT = process.cwd();
const LABEL = process.env['AXE_LABEL'] ?? 'current';
const OUT_DIR = path.join(ROOT, '.axe-audit', LABEL);
const BUILD_DIST = path.join(ROOT, 'app-dist');
const DIST = path.join(OUT_DIR, 'bundle');
const PORT = Number(process.env['AXE_PORT'] ?? 5241);
const MIME: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
  '.ttf': 'font/ttf',
  '.map': 'application/json; charset=utf-8',
  '.ico': 'image/x-icon',
};

export const STILLNESS_INIT = `(function () {
  var apply = function () {
    if (document.documentElement) {
      document.documentElement.setAttribute('data-motion', 'reduced');
    }
    if (document.head) {
      var style = document.createElement('style');
      style.textContent =
        '*,*::before,*::after{animation:none!important;transition:none!important;}';
      document.head.appendChild(style);
    }
  };
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', apply);
  } else {
    apply();
  }
})();`;

function serveDist(): Server {
  const server = createServer((req, res) => {
    const url = new URL(req.url ?? '/', 'http://localhost');
    let file = path.join(DIST, decodeURIComponent(url.pathname));
    if (!file.startsWith(DIST)) {
      res.writeHead(403).end();
      return;
    }
    if (!existsSync(file) || statSync(file).isDirectory()) {
      file = path.join(DIST, 'index.html');
    }
    res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] ?? 'application/octet-stream' });
    createReadStream(file).pipe(res);
  });
  server.listen(PORT, '127.0.0.1');
  return server;
}

function buildBundleIntoScratch(): void {
  console.log('[axe] building the bundle (the dev server can serve a crashed route) ...');
  // Snapshot the tracked output before building over it, and put it back as
  // soon as the fresh bundle has been copied somewhere private.
  const snapshot = path.join(OUT_DIR, 'app-dist.orig');
  const hadDist = existsSync(BUILD_DIST);
  if (hadDist) cpSync(BUILD_DIST, snapshot, { recursive: true });
  try {
    const build = spawnSync('npx', ['rsbuild', 'build'], {
      cwd: ROOT,
      env: { ...process.env, NO_COLOR: '1' },
      encoding: 'utf8',
    });
    if (build.status !== 0) {
      console.error(build.stdout ?? '', build.stderr ?? '');
      throw new Error(`rsbuild build failed with status ${String(build.status)}`);
    }
    if (!existsSync(path.join(BUILD_DIST, 'index.html'))) {
      throw new Error(`build produced no ${path.join(BUILD_DIST, 'index.html')}`);
    }
    cpSync(BUILD_DIST, DIST, { recursive: true });
  } finally {
    rmSync(BUILD_DIST, { recursive: true, force: true });
    if (hadDist) {
      cpSync(snapshot, BUILD_DIST, { recursive: true });
      rmSync(snapshot, { recursive: true, force: true });
    }
  }
}

export function startStaticServer(): { baseURL: string; server: Server } {
  buildBundleIntoScratch();
  const server = serveDist();
  const baseURL = `http://127.0.0.1:${PORT}`;
  console.log(`[axe] serving ${DIST} at ${baseURL}`);
  return { baseURL, server };
}
