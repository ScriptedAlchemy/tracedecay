import { defineConfig } from '@rsbuild/core';
import { pluginReact } from '@rsbuild/plugin-react';

// Single-app dashboard build (plan 11 "Finalized implementation architecture").
// Output goes to app-dist/ during the skeleton phase; the build.rs embedding
// integration (replacing the legacy placeholder dists) is a separate card and
// will point at this output when the shell is ready to serve.
export default defineConfig({
  plugins: [pluginReact()],
  source: {
    entry: { index: './src/app/main.tsx' },
  },
  html: {
    title: 'TraceDecay',
  },
  output: {
    distPath: { root: 'app-dist' },
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
    chunkSplit: { strategy: 'split-by-experience' },
  },
});
