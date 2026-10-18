import { useEffect, useRef } from "react";
import { mountFiringTree } from "./firingTreeScene";

export function FiringTree() {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { dispose } = mountFiringTree(el);
    return dispose;
  }, []);
  return <div className="firing-tree" ref={ref} />;
}
