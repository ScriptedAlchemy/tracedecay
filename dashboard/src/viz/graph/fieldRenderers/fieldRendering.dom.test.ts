import { afterEach, describe, expect, it, vi } from 'vitest';
import { ActivationField } from '../activation.ts';
import { createAtlasField, registryAtlas } from './atlas.ts';
import { createLuminousField } from './pointField.ts';
import { sampleFieldPalette, type FieldRenderer, type FieldRendererFactory, type FieldScene, type SceneBody } from './scene.ts';

function body(id: string, x: number): SceneBody {
  return { id, label: id, role: 'body', kind: 'primary', x, y: 1, radius: 0.1, mass: 12, units: { stores: 2, artifacts: 10 }, vitality: 1, detail: ['stores 2', 'artifacts 10', 'mass 12', 'seen 1m'], group: null, cluster: null };
}

function registry(bodies: SceneBody[], columns = 5): FieldScene {
  return { bodies, paths: [], clusters: [], extent: { x: [-0.5, columns - 0.5], y: [0, 2] }, columns: Array.from({ length: columns }, (_, index) => ({ label: `column ${index}`, bound: `${index}d`, count: bodies.filter((entry) => entry.x === index).length })), neighbors: new Map() };
}

const mounted: FieldRenderer[] = [];

function mount(create: FieldRendererFactory, scene: FieldScene, width: number, height: number) {
  const labels: Array<{ value: string; x: number; y: number; align: CanvasTextAlign }> = [];
  const arcs: Array<{ x: number; y: number; radius: number }> = [];
  const context = new Proxy({ textAlign: 'left' } as unknown as CanvasRenderingContext2D, {
    get: (target, property) => {
      if (property === 'fillText') return (value: string, x: number, y: number) => labels.push({ value, x, y, align: target.textAlign });
      if (property === 'arc') return (x: number, y: number, radius: number) => arcs.push({ x, y, radius });
      if (property === 'measureText') return (value: string) => ({ width: value.length * 6.6 });
      if (property === 'createRadialGradient') return () => ({ addColorStop: () => {} });
      if (property === 'getImageData') return () => ({ data: [80, 90, 100, 255] });
      return Reflect.get(target, property) ?? (() => {});
    },
  });
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation((kind: string) => (kind === '2d' ? context : null) as never);
  vi.stubGlobal('requestAnimationFrame', () => 1);
  vi.stubGlobal('cancelAnimationFrame', () => {});
  const container = document.createElement('div');
  Object.defineProperties(container, { clientWidth: { value: width }, clientHeight: { value: height } });
  document.body.appendChild(container);
  const field = new ActivationField();
  const renderer = create({ container, scene, field, palette: sampleFieldPalette(container), isReduced: () => true, onHover: vi.fn(), onSelect: vi.fn() });
  mounted.push(renderer);
  return { renderer, field, labels, arcs };
}

afterEach(() => {
  for (const renderer of mounted.splice(0)) renderer.destroy();
  document.body.innerHTML = '';
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('Brain canvas readings', () => {
  it('uses the narrow canvas for the field and keeps an inspected edge label readable', () => {
    const bodies = Array.from({ length: 5 }, (_, index) => body(`project-${index}`, index));
    bodies[4]!.label = 'rightmost-project-with-a-label-too-long-for-a-phone';
    const { renderer, arcs, labels } = mount(createLuminousField, registry(bodies), 320, 280);
    const centres = arcs.slice(0, bodies.length).map((arc) => arc.x);
    expect(Math.max(...centres) - Math.min(...centres)).toBeGreaterThan(160);
    labels.length = 0;
    renderer.setView({ inspected: 'project-4', focus: null });
    const label = labels.find((entry) => entry.value.startsWith('rightmost'))!;
    expect(label).toBeDefined();
    expect(label.x).toBeGreaterThanOrEqual(4);
    expect(label.x + label.value.length * 6.7).toBeLessThanOrEqual(316);
  });

  it('keeps a full Atlas reading inside its narrow plate', () => {
    const project = body('project', 0);
    project.label = 'project-with-a-long-registry-label';
    project.detail = ['stores 123,456', 'artifacts 987,654', 'mass 1,111,110', 'seen 1m'];
    const scene = registry([project], 1);
    const { labels, renderer, field } = mount(createAtlasField, scene, 240, 500);
    const plate = registryAtlas(scene, 240, 500).plates[0]!;
    const reading = labels.find((entry) => entry.value.startsWith('stores'))!;
    expect(reading.x + reading.value.length * 6.6).toBeLessThanOrEqual(plate.x + plate.w - 10);
    renderer.synapse({ from: project.id, to: null, at: performance.now(), label: 'ingest', time: '10:00:00' });
    field.strike([project.id]);
    labels.length = 0;
    renderer.setView({ inspected: project.id, focus: null });
    const heading = labels.find((entry) => entry.value.startsWith('project'))!;
    const activity = labels.find((entry) => entry.align === 'right' && entry.y === heading.y)!;
    expect(heading.x + heading.value.length * 6.6).toBeLessThan(activity.x - activity.value.length * 6.6);
  });

  it('prints the latest admitted activity for a repeatedly struck Atlas project', () => {
    const { renderer, field, labels } = mount(createAtlasField, registry([body('project', 0)], 1), 800, 500);
    renderer.synapse({ from: 'project', to: null, at: performance.now(), label: 'search', time: '10:00:00' });
    renderer.synapse({ from: 'project', to: null, at: performance.now(), label: 'ingest', time: '10:00:01' });
    field.strike(['project']);
    labels.length = 0;
    renderer.setView({ inspected: null, focus: null });
    expect(labels.some((entry) => entry.value === 'ingest · 10:00:01')).toBe(true);
    expect(labels.some((entry) => entry.value === 'search · 10:00:00')).toBe(false);
  });
});
