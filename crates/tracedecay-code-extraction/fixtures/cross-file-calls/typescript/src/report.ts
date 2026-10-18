import { mean } from "./math";
import * as u from "./util";

export function formatLine(value: string): string {
  return u.normalize(value) + "\n";
}

export function summary(values: number[]): string {
  return formatLine(String(mean(values)));
}
