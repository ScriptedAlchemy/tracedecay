/**
 * The seam between the temporal scene host and a renderer.
 *
 * The host owns everything a reader operates: the time-window toolbar, wheel
 * and drag, the minimap, the lane column, and one accessible SVG button per
 * event, bundle, encounter and toggle with the same labels and data
 * attributes in every renderer. A renderer owns only paint: the Canvas2D
 * substrate and the non-interactive marks inside those buttons. It receives
 * the laid-out model and the density summary and decides no coordinate, so
 * every renderer draws the same deterministic layout.
 */
import type { JSX } from 'react';
import type { SceneDensity } from '../density.ts';
import type { TemporalPalette } from '../palette.ts';
import type { EvidenceGrade, SceneCluster, SceneLane, SceneNode, TemporalSceneModel } from '../types.ts';

export type SceneRendererId = 'current' | 'rail' | 'weave' | 'strata';

export interface SceneFrame {
  readonly model: TemporalSceneModel;
  readonly density: SceneDensity | null;
  readonly fieldX0: number;
  readonly fieldX1: number;
  /** Top of the field, below the time ruler. */
  readonly top: number;
  readonly height: number;
  /** Label printed at the loaded tail, e.g. `NOW` or `LOADED END`. */
  readonly tailLabel: string;
}

export interface NodeMarkProps {
  readonly node: SceneNode;
  readonly hovered: boolean;
  readonly frame: SceneFrame;
}

export interface ClusterMarkProps {
  readonly cluster: SceneCluster;
  readonly lane: SceneLane;
  readonly frame: SceneFrame;
}

export interface SceneRenderer {
  readonly id: SceneRendererId;
  readonly label: string;
  /** Paint the substrate. Called once per model, density or palette change. */
  paint(ctx: CanvasRenderingContext2D, frame: SceneFrame, palette: TemporalPalette): void;
  /** Visual content of an event button; the host supplies role, label and hit area. */
  NodeMark(props: NodeMarkProps): JSX.Element;
  /** Visual content of a bundle button; the host supplies role, label and hit area. */
  ClusterMark(props: ClusterMarkProps): JSX.Element;
  /** Non-interactive marks over the field: cursor and loaded-tail markers
   * (carrying `data-cursor` and `data-tail-marker`), rail legends, grade tags. */
  FieldOverlay(props: { frame: SceneFrame }): JSX.Element;
  /** Second line in the lane column, or null to keep the provider line. */
  laneDetail?(lane: SceneLane, frame: SceneFrame): string | null;
  /** Legend swatch for a grade when the renderer paints grades as something
   * other than the shared line styles. */
  GradeSwatch?(props: { grade: EvidenceGrade }): JSX.Element;
  /** The renderer's own encodings (thickness, luminance, bar height), stated
   * in the legend so no channel goes unnamed. */
  LegendEncodings?(props: { frame: SceneFrame }): JSX.Element;
  /** Class on each event button; carries the focus-ring treatment. */
  readonly nodeClassName: string;
}
