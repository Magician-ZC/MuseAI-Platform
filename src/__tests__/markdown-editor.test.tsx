import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import MarkdownEditor from '../components/MarkdownEditor';
import { shouldPersist } from '../components/MarkdownEditorImpl';

const invokeMock = vi.mocked(invoke);

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
};

describe('MarkdownEditor', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'read_file') return '# 标题\n\n正文';
      if (command === 'file_modified_at') return 1;
      if (command === 'write_file') return 2;
      if (command === 'read_image_data_url') return 'data:image/png;base64,LOCAL';
      return undefined;
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('loads a large Markdown document into an editable CodeMirror source area', async () => {
    const largeMarkdown = Array.from({ length: 900 }, (_, index) => `## 第 ${index + 1} 节\n正文内容`).join('\n\n');
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'read_file') return largeMarkdown;
      if (command === 'file_modified_at') return 1;
      return undefined;
    });

    render(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/large.md" />);

    const editor = await screen.findByRole('textbox', { name: 'Markdown源码编辑区' }, { timeout: 3000 });
    expect((editor as HTMLTextAreaElement).value).toContain('第 900 节');
    expect(screen.getByTestId('markdown-live-editor')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '预览' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '分屏' })).not.toBeInTheDocument();
  });

  it('autosaves writable edits but does not save in read-only mode', async () => {
    const { unmount } = render(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/a.md" />);

    const editor = await screen.findByRole('textbox', { name: 'Markdown源码编辑区' });
    fireEvent.change(editor, { target: { value: '# 新标题\n\n新正文' } });

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 850));
    });

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('write_file', {
        path: '/Users/test/Documents/MuseAI/articles/a.md',
        content: '# 新标题\n\n新正文',
      });
    });

    unmount();
    invokeMock.mockClear();
    render(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/references/read-only.md" readOnly />);

    const readOnlyEditor = await screen.findByRole('textbox', { name: 'Markdown源码编辑区' });
    fireEvent.change(readOnlyEditor, { target: { value: '不应保存' } });

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 850));
    });

    expect(invokeMock).not.toHaveBeenCalledWith('write_file', expect.anything());
  });

  /**
   * 🔴 **打完最后一句、立刻点开下一章 —— 那一段字不许丢。**
   *
   * 防抖保存的清理函数是 `clearTimeout`，而它的依赖里有 `content`，也就是每敲一个键都
   * 重建一次计时器。所以只要在停止输入后 800ms 内换文件，这一轮打的字**一次都没落盘**，
   * 界面上还什么提示都没有。改动前这里实测 `write_file` 调用数为 **0**。
   */
  it('flushes pending edits when switching files inside the debounce window', async () => {
    const { rerender } = render(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/ch1.md" />);
    const editor = await screen.findByRole('textbox', { name: 'Markdown源码编辑区' });
    fireEvent.change(editor, { target: { value: '刚敲下的最后一段' } });

    // 远小于 800ms 的防抖窗口：此刻计时器还没到点。
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 100));
    });
    rerender(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/ch2.md" />);
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 1200));
    });

    expect(invokeMock).toHaveBeenCalledWith('write_file', {
      path: '/Users/test/Documents/MuseAI/articles/ch1.md',
      content: '刚敲下的最后一段',
    });
    // 🔴 补写必须落到**原来那个文件**上，不能被写进刚打开的 ch2。
    expect(invokeMock).not.toHaveBeenCalledWith('write_file', {
      path: '/Users/test/Documents/MuseAI/articles/ch2.md',
      content: '刚敲下的最后一段',
    });
  });

  /** 卸载（离开作品页 / 关窗）是同一个丢数据的窗口。 */
  it('flushes pending edits on unmount', async () => {
    const { unmount } = render(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/ch3.md" />);
    const editor = await screen.findByRole('textbox', { name: 'Markdown源码编辑区' });
    fireEvent.change(editor, { target: { value: '离开前的最后一句' } });
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 100));
    });

    unmount();
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 200));
    });

    expect(invokeMock).toHaveBeenCalledWith('write_file', {
      path: '/Users/test/Documents/MuseAI/articles/ch3.md',
      content: '离开前的最后一句',
    });
  });

  /**
   * 反向配对：**没有未保存改动时，离开不许写盘**。
   *
   * 只测「离开要补写」的话，把补写写成无条件 `write_file` 也能全绿——那会在每次切文件时
   * 重写一遍文件（刷新 mtime、无谓地触发 workspace-changed、还会和别处的写抢）。
   */
  it('does not write on leave when there is nothing unsaved', async () => {
    const { rerender } = render(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/clean1.md" />);
    await screen.findByRole('textbox', { name: 'Markdown源码编辑区' });
    invokeMock.mockClear();

    rerender(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/clean2.md" />);
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 200));
    });

    expect(invokeMock).not.toHaveBeenCalledWith('write_file', expect.anything());
  });

  /**
   * 🔴 读取失败时那句「**读取文件失败**: …」是**提示文案，不是正文**，绝不许被写回文件。
   *
   * 它同时压住两道锁：`readError` 与 `loadedPath === null`。
   */
  it('never writes the read-error placeholder back into the file', async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'read_file') throw new Error('磁盘炸了');
      if (command === 'file_modified_at') return 1;
      return undefined;
    });

    const { rerender } = render(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/broken.md" />);
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 200));
    });
    rerender(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/other.md" />);
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 1200));
    });

    expect(invokeMock).not.toHaveBeenCalledWith('write_file', expect.anything());
  });

  it('refreshes from disk when the file changes without unsaved edits', async () => {
    let modifiedAt = 1;
    let fileContent = '# 初始内容';
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'read_file') return fileContent;
      if (command === 'file_modified_at') return modifiedAt;
      return undefined;
    });

    render(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/refresh.md" />);

    expect(await screen.findByDisplayValue('# 初始内容')).toBeInTheDocument();

    fileContent = '# 外部更新';
    modifiedAt = 2;

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 1300));
    });

    expect(await screen.findByDisplayValue('# 外部更新')).toBeInTheDocument();
  });

  it('renders local and internet Markdown images inside the single editor without rewriting saved source', async () => {
    const markdown = [
      '# 图片段落',
      '',
      '![本地](./cover.png)',
      '',
      '<img src="preview.jpg" width="100%">',
      '',
      '[![Python](https://img.shields.io/badge/Python-3.10%2B-blue)](https://www.python.org/)',
    ].join('\n');
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'read_file') return markdown;
      if (command === 'file_modified_at') return 1;
      if (command === 'read_image_data_url') {
        return 'data:image/png;base64,LOCAL';
      }
      return undefined;
    });

    render(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/chapter.md" />);

    const liveEditor = await screen.findByTestId('markdown-live-editor');
    await waitFor(() => {
      expect(within(liveEditor).getByAltText('本地')).toHaveAttribute('src', 'data:image/png;base64,LOCAL');
      expect(within(liveEditor).getByAltText('preview.jpg')).toHaveAttribute('src', 'data:image/png;base64,LOCAL');
      expect(within(liveEditor).getByAltText('Python')).toHaveAttribute('src', 'https://img.shields.io/badge/Python-3.10%2B-blue');
    });
    const renderedEditor = liveEditor.querySelector('.cm-content');
    expect(renderedEditor).not.toHaveTextContent('<img src=');
    expect(renderedEditor).not.toHaveTextContent('](https://www.python.org/)');

    fireEvent.change(screen.getByRole('textbox', { name: 'Markdown源码编辑区' }), {
      target: { value: `${markdown}\n\n新增` },
    });
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 850));
    });

    expect(invokeMock).toHaveBeenCalledWith('write_file', {
      path: '/Users/test/Documents/MuseAI/articles/chapter.md',
      content: `${markdown}\n\n新增`,
    });
  });

  it('previews selected image files directly', async () => {
    render(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/cover.jpg" />);

    const image = await screen.findByAltText('cover.jpg');
    expect(image).toHaveAttribute('src', 'data:image/png;base64,LOCAL');
  });

  it('ignores stale text file loads after switching to another text file', async () => {
    const firstRead = deferred<string>();
    const firstModifiedAt = deferred<number>();
    invokeMock.mockImplementation((command: string, args?: any) => {
      if (command === 'read_file' && args?.path?.endsWith('/first.md')) return firstRead.promise;
      if (command === 'file_modified_at' && args?.path?.endsWith('/first.md')) return firstModifiedAt.promise;
      if (command === 'read_file' && args?.path?.endsWith('/second.md')) return Promise.resolve('# 第二篇');
      if (command === 'file_modified_at' && args?.path?.endsWith('/second.md')) return Promise.resolve(2);
      if (command === 'write_file') return Promise.resolve(3);
      return Promise.resolve(undefined);
    });

    const { rerender } = render(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/first.md" />);
    rerender(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/second.md" />);

    expect(await screen.findByDisplayValue('# 第二篇')).toBeInTheDocument();

    await act(async () => {
      firstRead.resolve('# 第一篇');
      firstModifiedAt.resolve(1);
      await Promise.resolve();
    });

    expect(screen.queryByDisplayValue('# 第一篇')).not.toBeInTheDocument();
    expect(screen.getByDisplayValue('# 第二篇')).toBeInTheDocument();
  });

  it('ignores stale text file loads after switching to image and empty selections', async () => {
    const textRead = deferred<string>();
    const textModifiedAt = deferred<number>();
    invokeMock.mockImplementation((command: string, args?: any) => {
      if (command === 'read_file' && args?.path?.endsWith('/slow.md')) return textRead.promise;
      if (command === 'file_modified_at' && args?.path?.endsWith('/slow.md')) return textModifiedAt.promise;
      if (command === 'read_image_data_url') return Promise.resolve('data:image/png;base64,IMAGE');
      if (command === 'write_file') return Promise.resolve(2);
      return Promise.resolve(undefined);
    });

    const { rerender } = render(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/slow.md" />);
    rerender(<MarkdownEditor filePath="/Users/test/Documents/MuseAI/articles/cover.png" />);

    const image = await screen.findByAltText('cover.png');
    expect(image).toHaveAttribute('src', 'data:image/png;base64,IMAGE');

    await act(async () => {
      textRead.resolve('# 过期正文');
      textModifiedAt.resolve(1);
      await Promise.resolve();
    });

    expect(screen.queryByDisplayValue('# 过期正文')).not.toBeInTheDocument();

    rerender(<MarkdownEditor filePath={null} />);
    expect(screen.getByText('选择左侧文件以开始阅读或编辑')).toBeInTheDocument();

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 850));
    });

    expect(invokeMock).not.toHaveBeenCalledWith('write_file', expect.anything());
  });
});

/**
 * 🔴 **「A 的正文绝不许被写进 B」——直接测判据本身。**
 *
 * `docs/VALIDATION.md` §3.47 欠账 A4：登记时我写的是「这条只能算纵深防御，
 * 故障注入证明不了它今天在挡什么（那一帧之后的重渲染远快于 800ms）」。
 * **那句话没走一遍就写了**——判据早就被抽成了纯函数 `shouldPersist`，
 * 直接调它就能把那一帧钉死，根本不需要构造「让重渲染慢过 800ms」。
 *
 * 那一帧是真实存在的：`text-load-start` 刻意保留上一个文件的 `content`
 * （换文件时编辑区不闪空白），于是「filePath 已是新文件、content 还是旧文件的」
 * 会真的出现一次。此前只有「重渲染比计时器快」在拦它。
 */
describe('shouldPersist（落盘判据本身）', () => {
  const base = {
    content: '新打的字',
    savedContent: '旧内容',
    imagePreviewSrc: '',
    loading: false,
    saveStatus: 'saving' as const,
    readError: false,
    loadedPath: '/w/a.md',
  };

  it('内容来自 a.md 时，绝不写进 b.md', () => {
    expect(shouldPersist(base, '/w/a.md', false)).toBe(true);
    expect(shouldPersist(base, '/w/b.md', false)).toBe(false);
  });

  it('内容不属于任何文件（加载中 / 读失败）时一个字节都不写', () => {
    expect(shouldPersist({ ...base, loadedPath: null }, '/w/a.md', false)).toBe(false);
    expect(shouldPersist({ ...base, loading: true }, '/w/a.md', false)).toBe(false);
    // 读失败时 content 是一句「**读取文件失败**: …」的提示文案，绝不是正文。
    expect(
      shouldPersist({ ...base, readError: true, content: '**读取文件失败**: boom', loadedPath: null }, '/w/a.md', false),
    ).toBe(false);
  });

  it('只读、无文件、图片一律不写；没有未保存改动也不写', () => {
    expect(shouldPersist(base, '/w/a.md', true)).toBe(false);
    expect(shouldPersist(base, null, false)).toBe(false);
    expect(shouldPersist({ ...base, loadedPath: '/w/a.png' }, '/w/a.png', false)).toBe(false);
    expect(shouldPersist({ ...base, content: base.savedContent }, '/w/a.md', false)).toBe(false);
  });
});
