import { render, screen, fireEvent } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import Settings from '../pages/Settings';

// agent-D4：Settings 为 14 个新环节 agentConfig 增加配置卡片（角色提取 6 / 知识 3 / 叙事 5）+ 按环节模型路由下拉。
// 三个分组懒挂载（标题与展开按钮常驻、卡片按需挂载），用最少的重量级渲染覆盖渲染 + 关键交互。
describe('Settings P0–P2 new-stage agent cards', () => {
  it('renders the three grouped sections with expand buttons wiring all 14 cards (6/3/5)', () => {
    render(<Settings />);
    // 侧边锚点 + 分组标题各出现一次。
    expect(screen.getAllByText('角色提取设置').length).toBeGreaterThanOrEqual(1);
    expect(screen.getAllByText('知识包设置').length).toBeGreaterThanOrEqual(1);
    expect(screen.getAllByText('叙事引擎设置').length).toBeGreaterThanOrEqual(1);
    // 展开按钮的计数即卡片数：6 + 3 + 5 = 14。
    expect(screen.getByRole('button', { name: '展开配置（6 项）' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '展开配置（3 项）' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '展开配置（5 项）' })).toBeInTheDocument();
    // 区块 id 就位（供侧边锚点跳转）。
    const container = document.getElementById('settings-scroll-container');
    expect(container?.querySelector('#character-extraction-config')).toBeTruthy();
    expect(container?.querySelector('#knowledge-config')).toBeTruthy();
    expect(container?.querySelector('#narrative-config')).toBeTruthy();
  });

  it('expanding a group mounts its cards with a per-stage model-routing dropdown', () => {
    render(<Settings />);
    // 展开最小的知识分组（3 项）以控制渲染负载。
    fireEvent.click(screen.getByRole('button', { name: '展开配置（3 项）' }));

    expect(screen.getByText('思维包蒸馏师')).toBeInTheDocument();
    expect(screen.getByText('价值包蒸馏师')).toBeInTheDocument();
    expect(screen.getByText('表达包蒸馏师')).toBeInTheDocument();
    // 每张卡带一个「按环节模型路由 (Model)」下拉。
    expect(screen.getAllByText('按环节模型路由 (Model)').length).toBe(3);
  });
});
