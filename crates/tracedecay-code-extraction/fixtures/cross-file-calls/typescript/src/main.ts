import * as compat from "./compat";
import * as legacy from "./legacy";
import { scale } from "./math";
import * as report from "./report";
import * as shapes from "./shapes";
import { Store } from "./store";

export function main(): void {
  const store = new Store();
  store.add("Key", 1);
  store.get("key");
  console.log(report.summary([1, 2, 3]));
  console.log(compat.upgrade(" Text "));
  console.log(shapes.area(3, 4), shapes.perimeter(3, 4));
  console.log(legacy.oldFormat("x"), scale(2, 3));
}
