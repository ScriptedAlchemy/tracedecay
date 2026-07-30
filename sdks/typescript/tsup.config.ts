import { defineConfig } from "tsup";

export default defineConfig({
  entry: {
    index: "src/index.ts",
    client: "src/client.ts",
  },
  clean: true,
  dts: true,
  format: ["esm"],
  sourcemap: true,
  splitting: false,
  target: "es2022",
});
