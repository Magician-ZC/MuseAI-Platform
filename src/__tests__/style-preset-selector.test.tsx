import { render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { StylePresetSelector } from '../components/StylePresetSelector';
import { useStylePresetStore } from '../stores/useStylePresetStore';

describe('文风预设单选器', () => {
  beforeEach(() => {
    useStylePresetStore.setState({
      selectedPresetId: null,
      presets: [
        { id: 'preset-a', name: '克制对白', segments: [{ id: 'a1', title: '对白', content: '一', enabled: true }], createdAt: 1, updatedAt: 1 },
        { id: 'preset-b', name: '电影感', segments: [{ id: 'b1', title: '镜头', content: '二二', enabled: true }], createdAt: 1, updatedAt: 1 },
      ],
    });
  });

  it('只显示一个当前预设且不提供预设排序按钮', () => {
    render(<StylePresetSelector target="adventure" value="preset-a" onChange={vi.fn()} sessionStarted />);
    expect(screen.getByText('克制对白')).toBeInTheDocument();
    expect(screen.getByText('生效正文 1 字')).toBeInTheDocument();
    expect(screen.queryByLabelText(/上移克制对白/)).not.toBeInTheDocument();
    expect(screen.getByText('修改仅影响后续回复，不会改变已有对话记录。')).toBeInTheDocument();
  });
});
