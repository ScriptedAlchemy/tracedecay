import { defineConfig } from "@rslib/core";

export default defineConfig({
  lib: [
    {
      format: "esm",
      syntax: "es2022",
      bundle: true,
      dts: true,
      source: {
        tsconfigPath: "./tsconfig.build.json",
        entry: {
          index: "src/index.ts",
          client: "src/client.ts",
        },
      },
      output: {
        sourceMap: true,
        minify: false,
      },
    },
  ],
});
