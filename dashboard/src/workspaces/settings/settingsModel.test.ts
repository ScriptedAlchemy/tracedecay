import { describe, expect, it } from 'vitest';
import { FIXTURES } from '../../../stories/fixtures/data.ts';
import {
  buildSettingsEditor,
  buildSettingsModel,
  countSettings,
  filterOverrides,
  filterRows,
  isPathLike,
  planProjectChangeAgainst,
  planUserChangeAgainst,
  readSettingsEnvelope,
  settingsRevisionConflict,
  splitPath,
} from './settingsModel.ts';

// `/api/settings` answers a DashboardEnvelopeV1; the read model addresses the
// settings groups inside its payload. Reading it through the generated
// contract also holds the fixture to the schema the daemon is generated from.
const settingsRead = readSettingsEnvelope(FIXTURES['/api/settings']);
if (settingsRead.outcome !== 'settings') {
  throw new Error(
    `the /api/settings fixture does not satisfy SettingsPayloadV1: ${settingsRead.outcome}`,
  );
}
const payload = settingsRead.payload;

// The change-planning tests below call `planProjectChangeAgainst` and
// `planUserChangeAgainst` because those are what the editor's reducer calls.
// Planning against an editor rather than a payload is also what makes the
// fixture's editability an explicit precondition instead of a null check
// repeated in every case.
const editor = buildSettingsEditor(payload);
if (!editor) {
  throw new Error('the /api/settings fixture must produce an editable snapshot');
}

describe('Settings read model', () => {
  it('reads every top-level group the payload carries', () => {
    const model = buildSettingsModel(payload);
    expect(model.sections.map((s) => s.id).sort()).toEqual([
      'automation',
      'environment',
      'project',
      'storage',
      'user',
      'version',
    ]);
  });

  it('orders file sources first, then the environment overlay, then resolved state', () => {
    const model = buildSettingsModel(payload);
    expect(model.sections.map((s) => s.origin)).toEqual([
      'file',
      'file',
      'environment',
      'resolved',
      'resolved',
      'resolved',
    ]);
  });

  // The honesty invariant this whole model exists to hold. `/api/settings`
  // ships effective values with no per-key attribution, so the model must not
  // expose anything a surface could render as "layer N overrode layer M".
  it('exposes no precedence or override ranking anywhere in the model', () => {
    const model = buildSettingsModel(payload);
    const serialized = JSON.stringify(model);
    expect(serialized).not.toMatch(/"rank"/);
    expect(serialized).not.toMatch(/overrides? (wins|beats)/i);
    for (const section of model.sections) {
      expect(section).not.toHaveProperty('rank');
      expect(section).not.toHaveProperty('precedence');
    }
  });

  it('states a source location only when the payload names one', () => {
    const model = buildSettingsModel(payload);
    const byId = Object.fromEntries(model.sections.map((s) => [s.id, s]));
    expect(byId['project']?.location).toBe(
      '/fast/projects/tracedecay/.tracedecay/config.toml',
    );
    expect(byId['project']?.locationKind).toBe('path');
    expect(byId['user']?.location).toBe('/home/zack/.tracedecay/config.toml');
    expect(byId['automation']?.locationKind).toBe('endpoint');
    // Storage and version state no config source, so none is invented.
    expect(byId['storage']?.location).toBeNull();
    expect(byId['version']?.location).toBeNull();
  });

  it('restates notes only from keys the payload actually carries', () => {
    const model = buildSettingsModel(payload);
    const project = model.sections.find((s) => s.id === 'project');
    expect(project?.notes).toContain('legacy config path is read-only');
    expect(project?.notes).toContain('config path and legacy path are the same file');
    // `user` carries `legacy_config_read_only` but not `config_path`, so only
    // the read-only note is restated.
    expect(model.sections.find((s) => s.id === 'user')?.notes).toEqual([
      'legacy config path is read-only',
    ]);
  });

  it('reads environment overrides verbatim, including explicit-vs-default state', () => {
    const model = buildSettingsModel(payload);
    expect(model.overrides).toHaveLength(2);
    expect(model.activeOverrides).toBe(0);
    // Unset: no value, so a default applies and none is invented.
    expect(model.overrides.find((o) => o.name === 'TRACEDECAY_DATA_DIR')).toMatchObject({
      active: false,
      value: null,
    });
  });

  it('treats a variable as in force only on a literal active:true', () => {
    const model = buildSettingsModel({
      environment: {
        variables: [
          { name: 'A', active: true, value: '1', description: 'd' },
          { name: 'B', active: 'true', value: '1', description: 'd' },
          { name: 'C', active: false, value: null, description: 'd' },
          { active: true, value: '1', description: 'nameless entries are dropped' },
        ],
      },
    });
    expect(model.overrides.map((o) => [o.name, o.active])).toEqual([
      ['A', true],
      ['B', false],
      ['C', false],
    ]);
    expect(model.activeOverrides).toBe(1);
  });

  it('does not double-report environment variables as generic rows', () => {
    const model = buildSettingsModel(payload);
    const environment = model.sections.find((s) => s.id === 'environment');
    expect(environment?.rows.some((row) => row.id.startsWith('variables'))).toBe(false);
    // The plain scalars in the same group are still reported.
    expect(environment?.rows.map((r) => r.label)).toContain('pricing_offline');
  });

  it('classifies leaf values by type', () => {
    const model = buildSettingsModel(payload);
    const rows = model.sections.find((s) => s.id === 'project')?.rows ?? [];
    const kind = (id: string) => rows.find((row) => row.id === id)?.kind;
    expect(kind('config_path')).toBe('path');
    expect(kind('legacy_config_read_only')).toBe('boolean');
    expect(kind('config.max_file_size')).toBe('number');
    expect(kind('config.include')).toBe('list');
    expect(kind('config')).toBe('group');
    expect(
      buildSettingsModel({ version: { cached_latest_version: null } }).sections[0]?.rows[0]
        ?.kind,
    ).toBe('null');
  });

  it('counts scalar settings and reports group subtree sizes', () => {
    const model = buildSettingsModel(payload);
    const project = model.sections.find((s) => s.id === 'project');
    const config = project?.rows.find((row) => row.id === 'config');
    expect(config?.kind).toBe('group');
    // include, exclude, max_file_size, extract_docstrings, track_call_sites,
    // git_ignore, telemetry.timings, sync.auto_track_pr_branches,
    // sync.auto_track_pr_poll_secs
    expect(config?.count).toBe(9);
    expect(countSettings(project?.rows ?? [])).toBe(project?.settingCount);
  });

  it('collects snapshot identity stamps the payload carries', () => {
    const model = buildSettingsModel(payload);
    expect(model.stamps).toEqual(
      expect.arrayContaining([
        { label: 'snapshot', value: 'snap-42' },
        { label: 'revision', value: 'rev-42' },
        { label: 'version', value: '2.0.0' },
        { label: 'channel', value: 'stable' },
      ]),
    );
  });

  it('returns an empty model for a payload that is not an object', () => {
    for (const bad of [null, undefined, 42, 'nope', []]) {
      const model = buildSettingsModel(bad);
      expect(model.sections).toEqual([]);
      expect(model.settingCount).toBe(0);
      expect(model.overrides).toEqual([]);
    }
  });

  it('renders unrecognized groups instead of dropping them', () => {
    const model = buildSettingsModel({ brand_new_group: { alpha: 1 } });
    expect(model.sections).toHaveLength(1);
    expect(model.sections[0]).toMatchObject({
      id: 'brand_new_group',
      title: 'Brand new group',
      origin: 'resolved',
    });
    expect(model.sections[0]?.settingCount).toBe(1);
  });

  it('does not describe unavailable automation authority as merged effective config', () => {
    const model = buildSettingsModel({
      automation: {
        config_endpoint: '/api/plugins/holographic/curation/config',
        availability: {
          available: false,
          reason: 'project automation configuration could not be read',
          required_authority: 'project automation configuration',
        },
        source_coverage: {
          global: 'available',
          project: 'error',
          effective: 'unavailable',
        },
      },
    });
    const automation = model.sections.find((section) => section.id === 'automation');

    expect(automation?.blurb).toBe('Automation configuration unavailable');
    expect(automation?.rows.map((row) => row.id)).not.toContain('enabled');
    expect(automation?.rows.map((row) => row.id)).not.toContain('backend');
    expect(automation?.rows.map((row) => row.id)).not.toContain('host_mode');
    expect(automation?.rows.find((row) => row.id === 'availability.reason')?.text).toBe(
      'project automation configuration could not be read',
    );
  });
});

describe('Settings filtering', () => {
  it('keeps ancestors of a matching row so nesting still reads', () => {
    const rows = buildSettingsModel(payload).sections.find((s) => s.id === 'project')!.rows;
    const filtered = filterRows(rows, 'auto_track_pr_poll_secs');
    expect(filtered.map((row) => row.id)).toEqual([
      'config',
      'config.sync',
      'config.sync.auto_track_pr_poll_secs',
    ]);
  });

  it('keeps the whole subtree of a group that matches by name', () => {
    const rows = buildSettingsModel(payload).sections.find((s) => s.id === 'project')!.rows;
    const filtered = filterRows(rows, 'telemetry');
    expect(filtered.map((row) => row.id)).toEqual([
      'config',
      'config.telemetry',
      'config.telemetry.timings',
    ]);
  });

  it('matches values as well as keys', () => {
    const rows = buildSettingsModel(payload).sections.find((s) => s.id === 'storage')!.rows;
    expect(filterRows(rows, 'graph.db').map((row) => row.id)).toEqual(['graph_db']);
  });

  it('returns every row for an empty or whitespace query', () => {
    const rows = buildSettingsModel(payload).sections.find((s) => s.id === 'user')!.rows;
    expect(filterRows(rows, '')).toHaveLength(rows.length);
    expect(filterRows(rows, '   ')).toHaveLength(rows.length);
  });

  it('filters overrides across name, value and description', () => {
    const overrides = buildSettingsModel(payload).overrides;
    expect(filterOverrides(overrides, 'DATA_DIR').map((o) => o.name)).toEqual([
      'TRACEDECAY_DATA_DIR',
    ]);
    expect(filterOverrides(overrides, 'pricing')).toEqual([]);
    expect(filterOverrides(overrides, '')).toHaveLength(2);
  });
});

describe('Settings value helpers', () => {
  it('recognizes absolute and home-relative paths only', () => {
    expect(isPathLike('/fast/projects/tracedecay')).toBe(true);
    expect(isPathLike('~/.tracedecay/config.toml')).toBe(true);
    expect(isPathLike('/')).toBe(false);
    expect(isPathLike('stable')).toBe(false);
    expect(isPathLike('/path with spaces')).toBe(false);
  });

  it('splits a path into directory prefix and final segment', () => {
    expect(splitPath('/a/b/c.toml')).toEqual({ head: '/a/b/', tail: 'c.toml' });
    expect(splitPath('bare')).toEqual({ head: '', tail: 'bare' });
  });
});

describe('Settings authorized changes', () => {
  it('captures the editable values and configuration revision from the GET payload', () => {
    expect(buildSettingsEditor(payload)).toEqual({
      projectExpectedRevisionId: 'rev-42',
      userExpectedRevisionId: 'user-rev-7',
      project: {
        include: ['src/**', 'dashboard/src/**'],
        exclude: ['target/**', 'node_modules/**'],
        max_file_size: '1048576',
        extract_docstrings: true,
        track_call_sites: true,
        git_ignore: true,
        telemetry_timings: false,
        auto_track_pr_branches: true,
        auto_track_pr_poll_secs: '120',
      },
      user: {
        upload_enabled: false,
        watcher_debounce: '2s',
        extraction_timeout_secs: '30',
      },
    });
  });

  it('builds a project patch containing only supported changed fields', () => {
    expect(
      planProjectChangeAgainst(editor, {
        ...editor.project,
        include: ['src/**', 'tests/**'],
        max_file_size: '2097152',
        telemetry_timings: true,
        auto_track_pr_poll_secs: '180',
      }),
    ).toEqual({
      outcome: 'ready',
      expectedRevisionId: 'rev-42',
      patch: {
        include: ['src/**', 'tests/**'],
        max_file_size: 2_097_152,
        telemetry: { timings: true },
        sync: { auto_track_pr_poll_secs: 180 },
      },
    });
  });

  it('rejects project values the backend validation rejects before a request', () => {
    const result = planProjectChangeAgainst(editor, {
      ...editor.project,
      include: [''],
      exclude: ['src/['],
      max_file_size: '0',
      auto_track_pr_poll_secs: '59',
    });

    expect(result).toMatchObject({
      outcome: 'invalid',
      errors: [
        { field: 'include', message: 'include patterns must not be empty' },
        { field: 'exclude', message: "invalid glob pattern 'src/['" },
        { field: 'max_file_size', message: 'max_file_size must be at least 1 byte' },
        {
          field: 'auto_track_pr_poll_secs',
          message: 'auto_track_pr_poll_secs must be at least 60 seconds',
        },
      ],
    });
  });

  it('matches the backend glob parser at escape and negated-class boundaries', () => {
    expect(
      planProjectChangeAgainst(editor, {
        ...editor.project,
        include: ['src/\\'],
      }).outcome,
    ).toBe('ready');
    expect(
      planProjectChangeAgainst(editor, {
        ...editor.project,
        include: ['[!]'],
      }),
    ).toMatchObject({
      outcome: 'invalid',
      errors: [{ field: 'include', message: "invalid glob pattern '[!]'" }],
    });
  });

  it('builds a user patch containing only supported changed fields', () => {
    expect(
      planUserChangeAgainst(editor, {
        ...editor.user,
        upload_enabled: true,
        watcher_debounce: '15s',
      }),
    ).toEqual({
      outcome: 'ready',
      expectedRevisionId: 'user-rev-7',
      patch: {
        upload_enabled: true,
        watcher_debounce: '15s',
      },
    });
  });

  it('rejects user values the backend validation rejects before a request', () => {
    expect(
      planUserChangeAgainst(editor, {
        ...editor.user,
        watcher_debounce: '1h',
        extraction_timeout_secs: '0',
      }),
    ).toEqual({
      outcome: 'invalid',
      errors: [
        {
          field: 'watcher_debounce',
          message: 'watcher_debounce must be a duration like "2s", "15s", or "1m"',
        },
        {
          field: 'extraction_timeout_secs',
          message: 'extraction_timeout_secs must be at least 1 second',
        },
      ],
    });
  });

  it('checks stale revisions against the mutated resource', () => {
    expect(settingsRevisionConflict('project', 'rev-41', payload)).toEqual({
      expectedRevisionId: 'rev-41',
      actualRevisionId: 'rev-42',
    });
    expect(settingsRevisionConflict('project', 'rev-42', payload)).toBeNull();
    expect(settingsRevisionConflict('user', 'user-rev-6', payload)).toEqual({
      expectedRevisionId: 'user-rev-6',
      actualRevisionId: 'user-rev-7',
    });
    expect(settingsRevisionConflict('user', 'user-rev-7', payload)).toBeNull();
  });
});
