import { hopped, relayed } from "../src/hops";

describe("hopped", () => {
  it("calls the import", () => {
    hopped();
  });
});

describe("relayed", () => {
  function relayed(): void {}
  it("calls the local helper", () => {
    relayed();
  });
});

it("calls the imported relay", () => {
  relayed();
});
