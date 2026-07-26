import { describe, expect, it } from "vitest";

import { OUTPUT_FILES } from "../src/generate.ts";

describe("contracts verification gate", () => {
  it("checks the live frontend contract instead of an unused preview", () => {
    expect(OUTPUT_FILES.GENERATED_FILE).toBe("src/contracts/generated.ts");
    expect(OUTPUT_FILES.INDEX_FILE).toBe("src/contracts/index.ts");
  });
});
