import { buildReport } from "../src/report.helpers";

describe("report helpers", () => {
  it("joins rows", () => {
    buildReport(["a"]);
    buildReport([]);
  });
});
