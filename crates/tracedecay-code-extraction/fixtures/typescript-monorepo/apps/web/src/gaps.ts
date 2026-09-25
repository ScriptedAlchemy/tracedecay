import { missing } from "./not-indexed";
import { gone } from "@fixture/shared/absent";
import { useState } from "react";

export function useGaps(): void {
  missing();
  gone();
  useState();
}
