import * as legacy from "./legacy";
import { normalize as norm } from "./util";

export function shim(text: string): string {
  return legacy.normalize(text);
}

export function upgrade(text: string): string {
  return norm(shim(text));
}
