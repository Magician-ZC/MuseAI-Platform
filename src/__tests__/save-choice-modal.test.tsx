import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { SaveChoiceModal, SaveChoiceOverwriteTarget } from '../components/SaveChoiceModal';

const initialTargets: SaveChoiceOverwriteTarget[] = [
  { value: 'progress-a', label: '进度甲' },
  { value: 'progress-b', label: '进度乙' },
];

const renderModal = (overrides: Partial<React.ComponentProps<typeof SaveChoiceModal>> = {}) => {
  const onConfirm = vi.fn();
  const props: React.ComponentProps<typeof SaveChoiceModal> = {
    open: true,
    title: '保存进度',
    nameLabel: '进度名称',
    initialName: '初始进度',
    overwriteAvailable: true,
    overwriteTargets: initialTargets,
    initialOverwriteTargetId: 'progress-b',
    onCancel: vi.fn(),
    onConfirm,
    ...overrides,
  };
  const result = render(<SaveChoiceModal {...props} />);
  return { ...result, onConfirm, props };
};

describe('SaveChoiceModal', () => {
  it('打开时将无效覆盖目标回退为第一个可用目标', () => {
    const { onConfirm } = renderModal({ initialOverwriteTargetId: 'missing' });

    fireEvent.click(screen.getByRole('button', { name: '确认保存' }));

    expect(onConfirm).toHaveBeenCalledWith({
      mode: 'overwrite',
      name: '初始进度',
      targetId: 'progress-a',
    });
  });

  it('目标值拼接结果碰撞时仍会重新校验并回退目标', () => {
    const firstTargets: SaveChoiceOverwriteTarget[] = [
      { value: 'a|b', label: '旧目标' },
      { value: 'c', label: '旧备用目标' },
    ];
    const nextTargets: SaveChoiceOverwriteTarget[] = [
      { value: 'a', label: '新目标' },
      { value: 'b|c', label: '新备用目标' },
    ];
    const { onConfirm, rerender, props } = renderModal({
      overwriteTargets: firstTargets,
      initialOverwriteTargetId: 'a|b',
    });

    rerender(
      <SaveChoiceModal
        {...props}
        overwriteTargets={nextTargets}
        initialOverwriteTargetId="a|b"
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: '确认保存' }));

    expect(onConfirm).toHaveBeenCalledWith({
      mode: 'overwrite',
      name: '初始进度',
      targetId: 'a',
    });
  });

  it('父组件以等价 props 重渲染时保留用户正在编辑的创建草稿', () => {
    const { onConfirm, rerender, props } = renderModal({
      overwriteAvailable: false,
      overwriteTargets: [],
      initialOverwriteTargetId: null,
    });
    const input = screen.getByRole('textbox', { name: '进度名称' });
    fireEvent.change(input, { target: { value: '  用户草稿  ' } });

    rerender(
      <SaveChoiceModal
        {...props}
        overwriteAvailable={false}
        overwriteTargets={[]}
        initialOverwriteTargetId={null}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: '确认保存' }));

    expect(onConfirm).toHaveBeenCalledWith({
      mode: 'create',
      name: '用户草稿',
      targetId: null,
    });
  });

  it('允许从覆盖模式切换到创建模式并提交裁剪后的名称', () => {
    const { onConfirm } = renderModal();

    fireEvent.click(screen.getByLabelText('保存为新记录'));
    fireEvent.change(screen.getByRole('textbox', { name: '进度名称' }), {
      target: { value: '  新分支  ' },
    });
    fireEvent.click(screen.getByRole('button', { name: '确认保存' }));

    expect(onConfirm).toHaveBeenCalledWith({
      mode: 'create',
      name: '新分支',
      targetId: 'progress-b',
    });
  });
});
