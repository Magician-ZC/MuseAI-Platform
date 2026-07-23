import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';
import { StylePresetEditor } from '../components/StylePresetEditor';
import { useStylePresetStore } from '../stores/useStylePresetStore';

describe('文风预设片段编辑器', () => {
  beforeEach(() => {
    useStylePresetStore.setState({
      selectedPresetId: 'preset-1',
      presets: [{
        id: 'preset-1',
        name: '综合文风',
        createdAt: 1,
        updatedAt: 1,
        segments: [
          { id: 'segment-1', title: '对白', content: '自然对白', enabled: true },
          { id: 'segment-2', title: '环境', content: '环境描写', enabled: true },
        ],
      }],
    });
  });

  it('默认收缩，点击标题展开内容', () => {
    render(<StylePresetEditor preset={useStylePresetStore.getState().presets[0]} />);
    expect(screen.queryByLabelText('提示词正文 1')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '对白' }));
    expect(screen.getByLabelText('提示词正文 1')).toHaveValue('自然对白');
    fireEvent.click(screen.getByRole('button', { name: '对白' }));
    expect(screen.queryByLabelText('提示词正文 1')).not.toBeInTheDocument();
  });

  it('使用上移下移按钮调整顺序并保留开关', () => {
    render(<StylePresetEditor preset={useStylePresetStore.getState().presets[0]} />);
    fireEvent.click(screen.getByLabelText('启用提示词 1'));
    expect(useStylePresetStore.getState().presets[0].segments[0].enabled).toBe(false);
    fireEvent.click(screen.getByLabelText('上移提示词 2'));
    expect(useStylePresetStore.getState().presets[0].segments.map((segment) => segment.title)).toEqual(['环境', '对白']);
    expect(screen.queryByLabelText(/拖拽/)).not.toBeInTheDocument();
  });

  it('独立维护多个片段的展开状态', () => {
    render(<StylePresetEditor preset={useStylePresetStore.getState().presets[0]} />);

    fireEvent.click(screen.getByRole('button', { name: '对白' }));
    fireEvent.click(screen.getByRole('button', { name: '环境' }));
    expect(screen.getByLabelText('提示词正文 1')).toHaveValue('自然对白');
    expect(screen.getByLabelText('提示词正文 2')).toHaveValue('环境描写');

    fireEvent.click(screen.getByRole('button', { name: '对白' }));
    expect(screen.queryByLabelText('提示词正文 1')).not.toBeInTheDocument();
    expect(screen.getByLabelText('提示词正文 2')).toHaveValue('环境描写');
  });
});
