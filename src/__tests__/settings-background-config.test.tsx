import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import Settings from '../pages/Settings';

describe('Settings background page configuration', () => {
  it('renders background extraction concurrency and model controls', async () => {
    render(<Settings />);

    expect(screen.getAllByText('背景页设置')[0]).toBeInTheDocument();
    expect(screen.getByLabelText('AI 提取背景设定并发数')).toBeInTheDocument();
    expect(screen.getByText('提取世界书')).toBeInTheDocument();
    expect(screen.getByText('提取角色卡')).toBeInTheDocument();
    expect(screen.getAllByText('温度').length).toBeGreaterThanOrEqual(2);
    expect(screen.getAllByText('最大上下文 Token').length).toBeGreaterThanOrEqual(2);
    expect(screen.getAllByText('思考深度 (Depth)').length).toBeGreaterThanOrEqual(2);
    expect(screen.getAllByText('系统提示词 (System Prompt)').length).toBeGreaterThanOrEqual(2);
  });
});
