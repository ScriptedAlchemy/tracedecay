import { defineConfig } from '@rsbuild/core';
import { pluginReact } from '@rsbuild/plugin-react';

// Canonical dashboard build. build.rs embeds this app-dist output into the
// binary served at `/`, including every client-routed workspace.
export default defineConfig({
  plugins: [pluginReact()],
  source: {
    entry: { index: './src/app/main.tsx' },
  },
  html: {
    title: 'TraceDecay',
    // axe: html-has-lang — the root element must declare its language, which
    // requires an explicit template; rsbuild's default omits `lang`.
    template: './src/app/index.html',
  },
  output: {
    distPath: { root: 'app-dist' },
    cleanDistPath: true,
    manifest: {
      filename: 'asset-manifest.json',
      prefix: false,
      filter: () => true,
    },
    // The daemon serves everything; assets must be relative and offline.
    assetPrefix: '/',
  },
  server: {
    port: 5173,
    proxy: {
      // Dev mode proxies API calls to a locally running daemon dashboard
      // server (`tracedecay dashboard --port 8321`). Override the target with
      // TRACEDECAY_DASHBOARD_API when verifying against another instance.
      '/api': process.env['TRACEDECAY_DASHBOARD_API'] ?? 'http://127.0.0.1:8321',
    },
  },
  performance: {
    // Persistent cache makes a warm production rebuild minify the same
    // entry to different short names than a cold build, so index.js
    // contenthash drifts and CI's consecutive-build sha256 check fails.
    buildCache: false,
    chunkSplit: { strategy: 'split-by-experience' },
  },
});
