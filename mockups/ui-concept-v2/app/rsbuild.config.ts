import { defineConfig } from "@rsbuild/core";
import { pluginReact } from "@rsbuild/plugin-react";

export default defineConfig({
  plugins: [pluginReact()],
  server: { port: 5195, host: "127.0.0.1" },
  html: {
    title: "TRACEDECAY · Brain",
    template: "./index.html",
  },
  output: { assetPrefix: "./", distPath: { root: "dist" } },
});
