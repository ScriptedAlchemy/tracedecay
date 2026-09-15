import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { ProjectInspector } from './ProjectInspector.tsx';

const project = {
  project_id: 'exact-project-id', label: 'project', project_root: '/alias',
  canonical_root: '/canonical/root', kind: 'worktree', store_count: 2,
  artifact_count: 5, alias_count: 1, branches: [], default_branch: null,
  last_seen_at: 1234567890,
};

describe('project inspection evidence', () => {
  it('shows exact registry identity and holdings without selecting scope', () => {
    const open = vi.fn();
    render(<ProjectInspector project={project} group={{ label: 'repo', git_common_dir: '/repo/.git', branches: [], project_count: 1, projects: [project] }} onClose={vi.fn()} onRepository={open} />);
    expect(screen.getByText('exact-project-id')).toBeTruthy();
    expect(screen.getByText('/canonical/root')).toBeTruthy();
    expect(screen.getByText('7')).toBeTruthy();
    expect(screen.getByText('1234567890 Unix seconds')).toBeTruthy();
    expect(open).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'View repository' }));
    expect(open).toHaveBeenCalledOnce();
  });

  it('does not invent a repository route when the registry has none', () => {
    render(<ProjectInspector project={project} group={{ label: 'ungrouped', git_common_dir: null, branches: [], project_count: 1, projects: [project] }} onClose={vi.fn()} onRepository={vi.fn()} />);
    expect(screen.getByText('not recorded')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'View repository' })).toBeNull();
  });
});
