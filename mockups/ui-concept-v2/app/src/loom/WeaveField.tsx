import type { EvidenceGrade, EventKind } from "./types";

export function gradeDash(g: EvidenceGrade): string | undefined {
  if (g === "exact" || g === "explicit") return undefined;
  if (g === "inferred") return "8 5";
  if (g === "ambiguous") return "3 4";
  if (g === "stale") return "2 4";
  return "1.5 5";
}

export function gradeColor(g: EvidenceGrade, lane = "#6cd7ed"): string {
  if (g === "ambiguous") return "#d6a65b";
  if (g === "stale") return "#cfac6b";
  if (g === "unavailable") return "#75828c";
  if (g === "inferred") return "#a68cd2";
  if (g === "explicit") return "#56cee9";
  return lane;
}

export function Glyph({ kind, color, s = 5 }: { kind: EventKind; color: string; s?: number }) {
  const sw = Math.max(1.1, s * 0.26);
  switch (kind) {
    case "commit":
    case "pr":
      return <polygon points={`0,${-s} ${s},0 0,${s} ${-s},0`} fill="none" stroke={color} strokeWidth={sw} />;
    case "branch":
      return <path d={`M0 ${s} L0 0 M0 0 L${-s} ${-s} M0 0 L${s} ${-s} M${-s} ${-s} m-1 0 a1 1 0 1 0 2 0`} fill="none" stroke={color} strokeWidth={sw} />;
    case "spawn":
      return <path d={`M0 ${-s} L0 ${s * 0.1} M0 ${s * 0.1} L${-s} ${s} M0 ${s * 0.1} L${s} ${s}`} fill="none" stroke={color} strokeWidth={sw} />;
    case "worktree":
    case "session":
      return <rect x={-s * 0.9} y={-s * 0.9} width={s * 1.8} height={s * 1.8} fill="none" stroke={color} strokeWidth={sw} />;
    case "end":
      return <path d={`M${-s * 0.9} ${-s * 0.9} H${s * 0.9} V${s * 0.9} H${-s * 0.9} Z M${s * 0.3} ${-s * 0.9} V${s * 0.9}`} fill="none" stroke={color} strokeWidth={sw} />;
    case "command":
      return <path d={`M${-s} ${-s * 0.7} L${-s * 0.2} 0 L${-s} ${s * 0.7} M${0} ${s * 0.8} H${s}`} fill="none" stroke={color} strokeWidth={sw} />;
    case "summary":
    case "file-read":
      return <path d={`M${-s * 0.7} ${-s} H${s * 0.4} L${s * 0.7} ${-s * 0.6} V${s} H${-s * 0.7} Z M${-s * 0.3} ${-s * 0.2} H${s * 0.3} M${-s * 0.3} ${s * 0.3} H${s * 0.3}`} fill="none" stroke={color} strokeWidth={sw} />;
    case "file-edit":
      return <path d={`M${-s} ${s} L${-s*.7} ${s*.3} L${s*.5} ${-s*.9} L${s*.9} ${-s*.5} L${-s*.3} ${s*.7} Z M${s*.25} ${-s*.65} L${s*.65} ${-s*.25}`} fill="none" stroke={color} strokeWidth={sw} strokeLinejoin="round" />;
    case "search":
      return <path d={`M${s * 0.35} ${s * 0.35} L${s} ${s} M${s * 0.35} ${s * 0.35} m2.2 0 a${s * 0.62} ${s * 0.62} 0 1 1 0.01 0`} fill="none" stroke={color} strokeWidth={sw} transform={`translate(${-s * 0.35},${-s * 0.35})`} />;
    case "decision":
    case "reasoning":
      return <path d={`M${-s * 0.8} ${s * 0.5} A ${s * 0.85} ${s * 0.85} 0 1 1 ${s * 0.8} ${s * 0.5} M${-s * 0.35} ${s * 0.9} H${s * 0.35}`} fill="none" stroke={color} strokeWidth={sw} />;
    case "tool":
      return <path d={`M${-s*.9} ${s*.6} L${s*.05} ${-s*.35} C${-s*.15} ${-s*.9} ${s*.4} ${-s*1.1} ${s*.75} ${-s*.85} L${s*.35} ${-s*.45} L${s*.6} ${-s*.2} L${s} ${-s*.6} C${s*1.15} ${s*.05} ${s*.55} ${s*.35} ${s*.2} ${s*.1} L${-s*.65} ${s*.95} Z`} fill="none" stroke={color} strokeWidth={sw} strokeLinejoin="round" />;
    case "gap":
      return <circle r={s * 0.9} fill="none" stroke={color} strokeWidth={sw} strokeDasharray="2 2.6" />;
    case "test":
      return <path d={`M${-s*.4} ${-s} H${s*.4} M${-s*.2} ${-s} V0 L${-s*.8} ${s} H${s*.8} L${s*.2} 0 V${-s}`} fill="none" stroke={color} strokeWidth={sw} />;
    case "result":
      return <path d={`M${-s} 0 L${-s*.2} ${s*.7} L${s} ${-s*.7}`} fill="none" stroke={color} strokeWidth={sw} />;
    case "handoff":
      return <path d={`M${-s} 0 H${s} M${s*.3} ${-s*.7} L${s} 0 L${s*.3} ${s*.7}`} fill="none" stroke={color} strokeWidth={sw} />;
    case "rejoin":
      return <><circle r={s} fill="none" stroke={color} strokeWidth={sw}/><circle r={s*.5} fill="none" stroke={color} strokeWidth={sw}/></>;
    case "task":
      return <><circle r={s*.7} fill="none" stroke={color} strokeWidth={sw}/><path d={`M0 ${-s} V${s} M${-s} 0 H${s}`} stroke={color} strokeWidth={sw}/></>;
    case "message":
      return <path d={`M${-s} ${-s*.7} H${s} V${s*.5} H0 L${-s*.65} ${s} V${s*.5} H${-s} Z M${-s*.55} ${-s*.2} H${s*.55}`} fill="none" stroke={color} strokeWidth={sw} strokeLinejoin="round" />;
    default: {
      const _exhaustive: never = kind;
      void _exhaustive;
      return null;
    }
  }
}
