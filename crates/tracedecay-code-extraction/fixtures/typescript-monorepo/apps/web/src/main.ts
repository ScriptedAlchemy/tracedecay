import { add, format } from "@fixture/shared";
import { sum } from "@fixture/shared/math";
import { buildReport } from "./report.helpers";
import { reexported } from "./lib";
import { widget } from "~/widgets/widget";

export function main(): string {
  format("a");
  add(1, 2);
  sum(3, 4);
  buildReport([]);
  reexported();
  widget();
  return format("b");
}
