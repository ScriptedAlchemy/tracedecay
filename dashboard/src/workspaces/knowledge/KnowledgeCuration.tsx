/**
 * Compatibility export for callers that still name the old facts-panel
 * component. Curation is now daemon-owned and the single implementation lives
 * in `CurationConsole`; there is no plan/apply UI behind this name.
 */
export { CurationConsole as KnowledgeCuration } from "./CurationConsole.tsx";
