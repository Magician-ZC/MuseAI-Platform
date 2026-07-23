import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { StylePresetManager } from '../components/StylePresetManager';
import { useStylePresetStore } from '../stores/useStylePresetStore';

describe('背景页文风预设目录', () => {
  beforeEach(() => useStylePresetStore.setState({ presets: [], selectedPresetId: null }));

  it('点击加号直接创建并选中空白预设，不弹出编辑弹窗', () => {
    const onSelectPreset = vi.fn();
    render(<StylePresetManager onSelectPreset={onSelectPreset} />);
    fireEvent.click(screen.getByLabelText('新增文风预设'));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.getByRole('treeitem', { name: /未命名预设/ })).toHaveAttribute('aria-selected', 'true');
    expect(useStylePresetStore.getState().presets[0].segments).toHaveLength(1);
    expect(onSelectPreset).toHaveBeenCalled();
  });
});
