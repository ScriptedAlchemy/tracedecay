import type { ReactElement } from "react";
import type { IconName } from "./surfaceChrome";

function strokeIcon(d: string | string[]) {
  const paths = Array.isArray(d) ? d : [d];
  return (
    <svg className="ico" viewBox="0 0 16 16" aria-hidden="true">
      {paths.map((p) => (
        <path key={p} d={p} fill="none" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" strokeLinejoin="round" />
      ))}
    </svg>
  );
}

export const SURFACE_ICONS: Record<IconName, ReactElement> = {
  link: strokeIcon([
    "M6.4 9.6a3.2 3.2 0 0 0 4.53 0l1.6-1.6a3.2 3.2 0 0 0-4.53-4.53l-.8.8",
    "M9.6 6.4a3.2 3.2 0 0 0-4.53 0l-1.6 1.6a3.2 3.2 0 1 0 4.53 4.53l.8-.8",
  ]),
  wifi: (
    <svg className="ico" viewBox="0 0 16 16" aria-hidden="true">
      <path d="M2.2 7.2a8.4 8.4 0 0 1 11.6 0" fill="none" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" />
      <path d="M4.4 9.2a5.4 5.4 0 0 1 7.2 0" fill="none" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" />
      <path d="M6.6 11.1a2.4 2.4 0 0 1 2.8 0" fill="none" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" />
      <circle cx="8" cy="13.1" r="0.9" fill="currentColor" />
    </svg>
  ),
  graph: (
    <svg className="ico" viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="8" cy="4.2" r="1.6" fill="none" stroke="currentColor" strokeWidth="1.25" />
      <circle cx="4.2" cy="12" r="1.6" fill="none" stroke="currentColor" strokeWidth="1.25" />
      <circle cx="12" cy="11.4" r="1.6" fill="none" stroke="currentColor" strokeWidth="1.25" />
      <path d="M7.1 5.5 5.1 10.5M8.9 5.6l2.2 4.3M5.8 12h4.4" fill="none" stroke="currentColor" strokeWidth="1.15" />
    </svg>
  ),
  scope: (
    <svg className="ico" viewBox="0 0 16 16" aria-hidden="true">
      <path
        fill="currentColor"
        fillRule="evenodd"
        d="M6.68 4.81 L7.15 2.87 L8.85 2.87 L9.32 4.81 L11.02 3.77 L12.23 4.98 L11.19 6.68 L13.13 7.15 L13.13 8.85 L11.19 9.32 L12.23 11.02 L11.02 12.23 L9.32 11.19 L8.85 13.13 L7.15 13.13 L6.68 11.19 L4.98 12.23 L3.77 11.02 L4.81 9.32 L2.87 8.85 L2.87 7.15 L4.81 6.68 L3.77 4.98 L4.98 3.77 L6.68 4.81 Z M9.70 8.00 A1.70 1.70 0 1 0 6.30 8.00 A1.70 1.70 0 1 0 9.70 8.00 Z"
      />
    </svg>
  ),
  pulse: strokeIcon("M1.5 8h2.2l1.2-3.2 2.2 6.4 1.6-3.2H14.5"),
  layers: strokeIcon(["M8 2.4 13.4 5 8 7.6 2.6 5 8 2.4Z", "M3 8.2 8 10.6 13 8.2", "M3 11.2 8 13.6 13 11.2"]),
  lock: strokeIcon(["M5.2 7.2V5.6a2.8 2.8 0 0 1 5.6 0v1.6", "M4.4 7.2h7.2v6.2H4.4Z"]),
  sliders: (
    <svg className="ico" viewBox="0 0 16 16" aria-hidden="true">
      <path d="M2 4.2h12M2 8h12M2 11.8h12" fill="none" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" />
      <circle cx="5.4" cy="4.2" r="1.15" fill="currentColor" />
      <circle cx="10.4" cy="8" r="1.15" fill="currentColor" />
      <circle cx="6.6" cy="11.8" r="1.15" fill="currentColor" />
    </svg>
  ),
  search: (
    <svg className="ico" viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="7" cy="7" r="3.4" fill="none" stroke="currentColor" strokeWidth="1.25" />
      <path d="M10.2 10.2 13.4 13.4" fill="none" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" />
    </svg>
  ),
  db: strokeIcon(["M3.2 4.2c0-1.2 2.1-2.2 4.8-2.2s4.8 1 4.8 2.2v7.6c0 1.2-2.1 2.2-4.8 2.2s-4.8-1-4.8-2.2Z", "M3.2 4.2c0 1.2 2.1 2.2 4.8 2.2s4.8-1 4.8-2.2", "M3.2 8c0 1.2 2.1 2.2 4.8 2.2s4.8-1 4.8-2.2"]),
  cloud: strokeIcon("M4.6 11.4h7.2A2.6 2.6 0 0 0 12 6.4 3.6 3.6 0 0 0 5.2 6.8 2.4 2.4 0 0 0 4.6 11.4Z"),
  shield: strokeIcon("M8 2.4 13.2 4.4v4.2c0 3.1-2.2 4.8-5.2 5.6-3-0.8-5.2-2.5-5.2-5.6V4.4L8 2.4Z"),
  camera: strokeIcon(["M3.2 5.2h2l1-1.4h3.6l1 1.4h2v7.2H3.2Z", "M8 11.2a2.2 2.2 0 1 0 0-4.4 2.2 2.2 0 0 0 0 4.4Z"]),
  hex: strokeIcon(["M8 2.2 13 5v6L8 13.8 3 11V5L8 2.2Z", "M8 5.2 11 6.8v3.2L8 11.6 5 10V6.8L8 5.2Z"]),
  page: strokeIcon(["M4.4 2.6h5.2L12 5.2v8.2H4.4Z", "M9.4 2.6v2.8H12"]),
  target: (
    <svg className="ico" viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="8" cy="8" r="5.2" fill="none" stroke="currentColor" strokeWidth="1.25" />
      <circle cx="8" cy="8" r="2.2" fill="none" stroke="currentColor" strokeWidth="1.25" />
      <path d="M8 2.2v1.8M8 12v1.8M2.2 8h1.8M12 8h1.8" fill="none" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" />
    </svg>
  ),
  inbox: strokeIcon(["M2.6 4.2h10.8v8H2.6Z", "M2.6 8.4 5.4 11h5.2l2.8-2.6"]),
  eye: strokeIcon(["M2.2 8c1.8-3.2 4-4.6 5.8-4.6S12 4.8 13.8 8c-1.8 3.2-4 4.6-5.8 4.6S4 11.2 2.2 8Z", "M8 9.6a1.6 1.6 0 1 0 0-3.2 1.6 1.6 0 0 0 0 3.2Z"]),
};
