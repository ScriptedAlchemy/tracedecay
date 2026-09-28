import { normalize } from "./util";

export class Store {
  private items = new Map<string, number>();

  add(key: string, value: number): void {
    this.items.set(normalize(key), value);
  }

  get(key: string): number | undefined {
    return this.items.get(normalize(key));
  }
}
