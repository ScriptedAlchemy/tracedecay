import { defineConfig } from "@rsbuild/core";

// Single self-contained HTML document: MCP App hosts load the resource text
// directly, so every script and style must be inlined.
export default defineConfig({
  source: { entry: { app: "./src/app/main.ts" } },
  html: { template: "./src/app/index.html", inject: "body" },
  output: {
    target: "web",
    // Committed artifacts must not inherit browser targets from ancestor directories.
    overrideBrowserslist: ["chrome >= 107", "edge >= 107", "firefox >= 104", "safari >= 16"],
    distPath: { root: "dist/app" },
    inlineScripts: true,
    inlineStyles: true,
    filenameHash: false,
    sourceMap: { js: false, css: false },
    legalComments: "none",
  },
  performance: { chunkSplit: { strategy: "all-in-one" } },
  tools: {
    htmlPlugin: { minify: false },
    rspack: { output: { asyncChunks: false } },
  },
});
