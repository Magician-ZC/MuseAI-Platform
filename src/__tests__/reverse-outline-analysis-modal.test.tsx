import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ReverseOutlineAnalysisModal from '../components/ReverseOutlineAnalysisModal';
import { useSettingsStore } from '../stores/useSettingsStore';

type EventHandler = (event: { payload: any }) => void;

const eventHandlers: Record<string, EventHandler> = {};

const defaultInvoke = async (command: string, args?: any) => {
  if (command === 'get_workspace_dir') {
    return args.dirType === 'articles'
      ? '/Users/test/Documents/MuseAI/articles'
      : '/Users/test/Documents/MuseAI/references';
  }
  if (command === 'list_dir' && args.path.endsWith('/articles')) {
    return [
      { name: '短篇合集', path: '/Users/test/Documents/MuseAI/articles/短篇合集', is_dir: true },
      { name: '正文B.md', path: '/Users/test/Documents/MuseAI/articles/正文B.md', is_dir: false },
      { name: '封面.png', path: '/Users/test/Documents/MuseAI/articles/封面.png', is_dir: false },
    ];
  }
  if (command === 'list_dir' && args.path.endsWith('/articles/短篇合集')) {
    return [
      { name: '正文A.md', path: '/Users/test/Documents/MuseAI/articles/短篇合集/正文A.md', is_dir: false },
    ];
  }
  if (command === 'list_dir' && args.path.endsWith('/references')) {
    return [
      { name: '女频', path: '/Users/test/Documents/MuseAI/references/女频', is_dir: true },
      { name: '范文A.txt', path: '/Users/test/Documents/MuseAI/references/范文A.txt', is_dir: false },
    ];
  }
  if (command === 'list_dir' && args.path.endsWith('/references/女频')) {
    return [
      { name: '范文B.txt', path: '/Users/test/Documents/MuseAI/references/女频/范文B.txt', is_dir: false },
    ];
  }
  if (command === 'preview_reverse_outline_chapters') {
    return [
      { title: '范文A', path: '/Users/test/Documents/MuseAI/references/范文A.txt', charCount: 1200 },
      { title: '正文B', path: '/Users/test/Documents/MuseAI/articles/正文B.md', charCount: 2400 },
    ];
  }
  if (command === 'start_reverse_outline_analysis') {
    return { runId: 'reverse-run-1' };
  }
  if (command === 'retry_and_finalize_reverse_outline') {
    return { runId: 'reverse-run-2' };
  }
  if (command === 'save_reverse_outline') {
    return { path: '/Users/test/Documents/MuseAI/outline/反向大纲.md' };
  }
  return undefined;
};

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
};

const invokeMock = vi.fn(defaultInvoke);

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string, args?: unknown) => invokeMock(command, args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: async (event: string, handler: EventHandler) => {
    eventHandlers[event] = handler;
    return () => {
      delete eventHandlers[event];
    };
  },
}));

const renderModal = () => render(<ReverseOutlineAnalysisModal open onClose={vi.fn()} />);

describe('ReverseOutlineAnalysisModal', () => {
  beforeEach(() => {
    invokeMock.mockClear();
    invokeMock.mockImplementation(defaultInvoke);
    Object.keys(eventHandlers).forEach(key => delete eventHandlers[key]);
    useSettingsStore.setState({
      llmProvider: 'OpenAI',
      modelInterface: 'OpenAI-compatible',
      llmBaseUrl: 'https://api.example.test/v1',
      llmApiKey: 'test-key',
      llmModel: 'test-model',
      models: [
        {
          id: 'test-model-id',
          name: '测试模型',
          provider: 'OpenAI',
          modelInterface: 'OpenAI-compatible',
          baseUrl: 'https://api.example.test/v1',
          apiKey: 'test-key',
          model: 'test-model',
        },
      ],
      selectedModelId: 'test-model-id',
      reverseOutlineShortPrompt: '短篇提示词',
      reverseOutlineLongSummaryPrompt: '分段摘要提示词',
      reverseOutlineLongFinalPrompt: '汇总大纲提示词',
      agentConfigs: {
        ...useSettingsStore.getState().agentConfigs,
        reverseOutline: { concurrency: 5 },
        reverseOutlineShort: { temperature: 0.3, maxOutputTokens: 32000, maxContextTokens: 200000, thinkingDepth: 'off' },
        reverseOutlineLongSummary: { temperature: 0.3, maxOutputTokens: 8192, maxContextTokens: 200000, thinkingDepth: 'off' },
        reverseOutlineLongFinal: { temperature: 0.3, maxOutputTokens: 32000, maxContextTokens: 200000, thinkingDepth: 'off' },
      },
    });
  });

  it('opens with source selection, validates required selection, and switches article type', async () => {
    renderModal();

    expect(screen.getByText('AI反向分析大纲')).toBeInTheDocument();
    await waitFor(() => expect(screen.getByLabelText('正文B')).toBeInTheDocument());
    expect(screen.getByText('短篇合集')).toBeInTheDocument();
    expect(screen.getByText('女频')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '开始分析' })).toBeDisabled();

    fireEvent.click(screen.getByLabelText('正文B'));
    expect(screen.getByText('已选择作品 1 篇，范文 0 篇')).toBeInTheDocument();

    fireEvent.click(screen.getByText('长篇'));
    await waitFor(() => expect(screen.getByText('此为字母顺序排列，如果顺序不对，请重命名文件')).toBeInTheDocument());
    expect(screen.getByText('我已确认章节顺序正确')).toBeInTheDocument();
  });

  it('requires long-form order confirmation and renders distributed progress', async () => {
    renderModal();

    await waitFor(() => expect(screen.getByLabelText('正文B')).toBeInTheDocument());
    fireEvent.click(screen.getByLabelText('正文B'));
    fireEvent.click(screen.getByLabelText('范文A'));
    expect(screen.getByLabelText('正文B')).toBeChecked();
    expect(screen.getByLabelText('范文A')).toBeChecked();
    fireEvent.click(screen.getByText('长篇'));

    await waitFor(() => expect(screen.getByText('1. 范文A')).toBeInTheDocument());
    expect(screen.getByRole('button', { name: '开始分析' })).toBeDisabled();

    fireEvent.click(screen.getByLabelText('我已确认章节顺序正确'));
    fireEvent.click(screen.getByRole('button', { name: '开始分析' }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        'start_reverse_outline_analysis',
        expect.objectContaining({
          request: expect.objectContaining({ articleType: 'long' }),
        }),
      );
    });

    await act(async () => {
      eventHandlers['reverse-outline-progress']?.({
        payload: {
          runId: 'reverse-run-1',
          phase: 'distributed',
          totalChapters: 2,
          successChapters: 1,
          failedChapters: 0,
        },
      });
    });

    expect(screen.getByText('总批次：2')).toBeInTheDocument();
    expect(screen.getByText('成功：1')).toBeInTheDocument();
    expect(screen.getByText('失败：0')).toBeInTheDocument();
  });

  it('passes configured system prompts for all reverse-outline stages', async () => {
    renderModal();

    await waitFor(() => expect(screen.getByLabelText('正文B')).toBeInTheDocument());
    fireEvent.click(screen.getByLabelText('正文B'));
    fireEvent.click(screen.getByText('长篇'));
    fireEvent.click(screen.getByLabelText('我已确认章节顺序正确'));
    fireEvent.click(screen.getByRole('button', { name: '开始分析' }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        'start_reverse_outline_analysis',
        expect.objectContaining({
          request: expect.objectContaining({
            systemPrompt: '分段摘要提示词',
            shortConfig: expect.objectContaining({ systemPrompt: '短篇提示词' }),
            longSummaryConfig: expect.objectContaining({ systemPrompt: '分段摘要提示词' }),
            longFinalConfig: expect.objectContaining({ systemPrompt: '汇总大纲提示词' }),
          }),
        }),
      );
    });
  });

  it('streams final outline text while long-form summary is running', async () => {
    renderModal();

    await waitFor(() => expect(screen.getByLabelText('正文B')).toBeInTheDocument());
    fireEvent.click(screen.getByLabelText('正文B'));
    fireEvent.click(screen.getByText('长篇'));
    fireEvent.click(screen.getByLabelText('我已确认章节顺序正确'));
    fireEvent.click(screen.getByRole('button', { name: '开始分析' }));

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('start_reverse_outline_analysis', expect.anything()));

    await act(async () => {
      eventHandlers['reverse-outline-progress']?.({
        payload: {
          runId: 'reverse-run-1',
          phase: 'final',
          totalChapters: 2,
          successChapters: 2,
          failedChapters: 0,
          message: '正在汇总生成长篇反向大纲',
        },
      });
      eventHandlers['reverse-outline-stream']?.({
        payload: {
          runId: 'reverse-run-1',
          delta: '# 长篇反向大纲\n',
        },
      });
      eventHandlers['reverse-outline-stream']?.({
        payload: {
          runId: 'reverse-run-1',
          delta: '一句话卖点：测试卖点',
        },
      });
    });

    expect(screen.getByText('正在汇总生成长篇反向大纲')).toBeInTheDocument();
    expect(screen.getByText(/一句话卖点：测试卖点/)).toBeInTheDocument();
  });

  it('passes selected directories to backend so folders are expanded there', async () => {
    renderModal();

    await waitFor(() => expect(screen.getByText('短篇合集')).toBeInTheDocument());
    const directoryNode = screen.getByText('短篇合集').closest('.ant-tree-treenode');
    const directoryCheckbox = directoryNode?.querySelector('.ant-tree-checkbox') as HTMLElement;
    fireEvent.click(directoryCheckbox);

    expect(screen.getByText('已选择作品 1 篇，范文 0 篇')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '开始分析' }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        'start_reverse_outline_analysis',
        expect.objectContaining({
          request: expect.objectContaining({
            filePaths: ['/Users/test/Documents/MuseAI/articles/短篇合集'],
          }),
        }),
      );
    });
  });

  it('shows generated preview, lets the user edit it, and saves through backend command', async () => {
    renderModal();

    await waitFor(() => expect(screen.getByLabelText('正文B')).toBeInTheDocument());
    fireEvent.click(screen.getByLabelText('正文B'));
    fireEvent.click(screen.getByRole('button', { name: '开始分析' }));

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('start_reverse_outline_analysis', expect.anything()));

    await act(async () => {
      eventHandlers['reverse-outline-result']?.({
        payload: {
          runId: 'reverse-run-1',
          title: '短篇反向大纲',
          content: '- 文章类型和标签\n- 导语',
        },
      });
    });

    fireEvent.change(screen.getByLabelText('大纲标题'), { target: { value: '新的反向大纲' } });
    fireEvent.change(screen.getByLabelText('大纲内容'), { target: { value: '调整后的大纲内容' } });
    fireEvent.click(screen.getByRole('button', { name: /保存到大纲目录/ }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('save_reverse_outline', {
        request: {
          title: '新的反向大纲',
          content: '调整后的大纲内容',
        },
      });
    });
  });

  it('shows failed batch reasons and allows finalizing with successful summaries', async () => {
    renderModal();

    await waitFor(() => expect(screen.getByLabelText('正文B')).toBeInTheDocument());
    fireEvent.click(screen.getByLabelText('正文B'));
    fireEvent.click(screen.getByLabelText('长篇'));
    fireEvent.click(screen.getByLabelText('我已确认章节顺序正确'));
    fireEvent.click(screen.getByRole('button', { name: '开始分析' }));

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('start_reverse_outline_analysis', expect.anything()));

    await act(async () => {
      eventHandlers['reverse-outline-progress']?.({
        payload: {
          runId: 'reverse-run-1',
          phase: 'distributed',
          totalChapters: 2,
          successChapters: 1,
          failedChapters: 1,
        },
      });
      eventHandlers['reverse-outline-result']?.({
        payload: {
          runId: 'reverse-run-1',
          error: '部分段落分析失败',
          failedBatchIndices: [1],
          failedBatchErrors: [
            {
              index: 1,
              range: '11-20',
              error: 'OpenAI 兼容接口请求失败：451 censorship_blocked',
            },
          ],
          partialSummaries: [
            { batchIndex: 0, 段落序号: '1-10', 剧情概要: '前十段概要' },
          ],
        },
      });
    });

    expect(screen.getByText('批次 2（段落 11-20）')).toBeInTheDocument();
    expect(screen.getByText('OpenAI 兼容接口请求失败：451 censorship_blocked')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: /继续汇总已成功段落/ }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        'retry_and_finalize_reverse_outline',
        expect.objectContaining({
          request: expect.objectContaining({
            failedBatchIndices: [],
            partialSummaries: [
              { batchIndex: 0, 段落序号: '1-10', 剧情概要: '前十段概要' },
            ],
          }),
        }),
      );
    });
  });

  it('starts from clean setup state when reopened by parent state', async () => {
    const { rerender } = render(<ReverseOutlineAnalysisModal open onClose={vi.fn()} />);

    await waitFor(() => expect(screen.getByLabelText('正文B')).toBeInTheDocument());
    fireEvent.click(screen.getByLabelText('正文B'));
    expect(screen.getByText('已选择作品 1 篇，范文 0 篇')).toBeInTheDocument();

    rerender(<ReverseOutlineAnalysisModal open={false} onClose={vi.fn()} />);
    rerender(<ReverseOutlineAnalysisModal open onClose={vi.fn()} />);

    await waitFor(() => expect(screen.getByLabelText('正文B')).toBeInTheDocument());
    expect(screen.queryByText('已选择作品 1 篇，范文 0 篇')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: '开始分析' })).toBeDisabled();
    expect(screen.getByText('短篇')).toBeInTheDocument();
  });

  it('ignores stale long-form previews after selected files change and clears them in short mode', async () => {
    const firstPreview = deferred<Array<{ title: string; path: string; charCount: number }>>();
    const secondPreview = deferred<Array<{ title: string; path: string; charCount: number }>>();
    let previewCallCount = 0;
    invokeMock.mockImplementation((command: string, args?: any) => {
      if (command === 'preview_reverse_outline_chapters') {
        previewCallCount += 1;
        return previewCallCount === 1 ? firstPreview.promise : secondPreview.promise;
      }
      return defaultInvoke(command, args);
    });

    renderModal();

    await waitFor(() => expect(screen.getByLabelText('正文B')).toBeInTheDocument());
    fireEvent.click(screen.getByLabelText('正文B'));
    fireEvent.click(screen.getByText('长篇'));

    await waitFor(() => expect(previewCallCount).toBe(1));
    fireEvent.click(screen.getByLabelText('范文A'));
    await waitFor(() => expect(previewCallCount).toBe(2));

    await act(async () => {
      secondPreview.resolve([
        { title: '当前章节', path: '/Users/test/Documents/MuseAI/references/范文A.txt', charCount: 2200 },
      ]);
      await Promise.resolve();
    });

    expect(await screen.findByText('1. 当前章节')).toBeInTheDocument();

    await act(async () => {
      firstPreview.resolve([
        { title: '过期章节', path: '/Users/test/Documents/MuseAI/articles/正文B.md', charCount: 1100 },
      ]);
      await Promise.resolve();
    });

    expect(screen.queryByText('1. 过期章节')).not.toBeInTheDocument();
    expect(screen.getByText('1. 当前章节')).toBeInTheDocument();

    fireEvent.click(screen.getByText('短篇'));

    expect(screen.queryByText('1. 当前章节')).not.toBeInTheDocument();
    expect(screen.queryByLabelText('我已确认章节顺序正确')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: '开始分析' })).toBeEnabled();
  });
});
