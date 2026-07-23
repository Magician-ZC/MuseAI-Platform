import { fireEvent, render, screen } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { MemoryRouter } from 'react-router-dom';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import DeAi from '../pages/DeAi';
import Examples from '../pages/Examples';
import Outline from '../pages/Outline';
import Works from '../pages/Works';
import { useDeAiStore } from '../stores/useDeAiStore';
import { useOutlineStore } from '../stores/useOutlineStore';
import { useWorksStore } from '../stores/useWorksStore';

const invokeMock = vi.mocked(invoke);

vi.mock('../components/WorkspaceDirectory', () => ({
  default: ({ title }: { title: string }) => <div>{title}</div>,
}));

vi.mock('../components/MarkdownEditor', () => ({
  default: ({ filePath, readOnly }: { filePath: string | null; readOnly?: boolean }) => (
    <div data-testid="shared-markdown-editor" data-file-path={filePath || ''} data-read-only={readOnly ? 'true' : 'false'} />
  ),
}));

vi.mock('../components/AgentChat', () => ({ default: () => <div>Agent</div> }));
vi.mock('../components/DeAiAgentChat', () => ({ default: () => <div>去AI味 Agent</div> }));
vi.mock('../components/OutlineAssessmentAgentChat', () => ({ default: () => <div>大纲评估 Agent</div> }));
vi.mock('../components/OutlineCreationAgentChat', () => ({ default: () => <div>大纲制作 Agent</div> }));
vi.mock('../components/ScoreDetailsModal', () => ({ ScoreDetailsModal: () => null }));

describe('Markdown editor page integration', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'list_files') return [];
      if (command === 'list_file_versions') return [];
      if (command === 'list_file_versions_with_scores') return [];
      return undefined;
    });
    useWorksStore.setState({ selectedFile: '/Users/test/Documents/MuseAI/articles/work.md' });
    useOutlineStore.setState({
      selectedOutlineFile: '/Users/test/Documents/MuseAI/outline/outline.md',
      fileTreeWidth: 280,
      agentWidth: 420,
      isAgentVisible: true,
    });
    useDeAiStore.setState({
      selectedWorkFile: '/Users/test/Documents/MuseAI/articles/de-ai.md',
      selectedReferenceFile: '/Users/test/Documents/MuseAI/references/example.md',
      activePreviewFile: '/Users/test/Documents/MuseAI/articles/de-ai.md',
    });
  });

  it('mounts the shared editor on the Works page', () => {
    render(<Works />);

    expect(screen.getByTestId('shared-markdown-editor')).toHaveAttribute(
      'data-file-path',
      '/Users/test/Documents/MuseAI/articles/work.md',
    );
    expect(screen.getByRole('separator', { name: '调整文件树宽度' })).toBeInTheDocument();
  });

  it('mounts the shared editor on the Outline page', () => {
    render(
      <MemoryRouter>
        <Outline />
      </MemoryRouter>
    );

    expect(screen.getByTestId('shared-markdown-editor')).toHaveAttribute(
      'data-file-path',
      '/Users/test/Documents/MuseAI/outline/outline.md',
    );
    expect(screen.getByRole('separator', { name: '调整文件树宽度' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '大纲评分暂无' })).toBeInTheDocument();
  });

  it('keeps Outline divider drags mutually exclusive and clamps both widths', () => {
    render(
      <MemoryRouter>
        <Outline />
      </MemoryRouter>
    );

    const fileTreeSeparator = screen.getByRole('separator', { name: '调整文件树宽度' });
    const agentSeparator = screen.getByRole('separator', { name: '调整 Agent 宽度' });

    fireEvent.mouseDown(fileTreeSeparator);
    expect(document.body.style.cursor).toBe('col-resize');
    expect(document.body.style.userSelect).toBe('none');

    fireEvent.mouseMove(window, { clientX: 100 });
    expect(useOutlineStore.getState().fileTreeWidth).toBe(250);
    fireEvent.mouseMove(window, { clientX: 1000 });
    expect(useOutlineStore.getState().fileTreeWidth).toBe(420);

    fireEvent.mouseDown(agentSeparator);
    fireEvent.mouseMove(window, { clientX: 1000 });
    expect(useOutlineStore.getState().fileTreeWidth).toBe(420);
    expect(useOutlineStore.getState().agentWidth).toBe(380);

    fireEvent.mouseMove(window, { clientX: 0 });
    expect(useOutlineStore.getState().agentWidth).toBe(860);

    fireEvent.mouseUp(window);
    expect(document.body.style.cursor).toBe('');
    expect(document.body.style.userSelect).toBe('');

    fireEvent.mouseMove(window, { clientX: 600 });
    expect(useOutlineStore.getState().agentWidth).toBe(860);
  });

  it('cleans up an active Outline drag when the page unmounts', () => {
    const { unmount } = render(
      <MemoryRouter>
        <Outline />
      </MemoryRouter>
    );

    fireEvent.mouseDown(screen.getByRole('separator', { name: '调整文件树宽度' }));
    fireEvent.mouseMove(window, { clientX: 360 });
    expect(useOutlineStore.getState().fileTreeWidth).toBe(360);

    unmount();
    expect(document.body.style.cursor).toBe('');
    expect(document.body.style.userSelect).toBe('');

    fireEvent.mouseMove(window, { clientX: 260 });
    expect(useOutlineStore.getState().fileTreeWidth).toBe(360);
  });

  it('mounts the shared editor on the De-AI page', () => {
    render(<DeAi />);

    expect(screen.getByTestId('shared-markdown-editor')).toHaveAttribute(
      'data-file-path',
      '/Users/test/Documents/MuseAI/articles/de-ai.md',
    );
    expect(screen.getByRole('separator', { name: '调整目录宽度' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'AI味评分暂无' })).toBeInTheDocument();
  });

  it('mounts the shared editor on the Examples page', () => {
    render(<Examples />);

    expect(screen.getByTestId('shared-markdown-editor')).toHaveAttribute(
      'data-file-path',
      '/Users/test/Documents/MuseAI/references/example.md',
    );
  });
});
