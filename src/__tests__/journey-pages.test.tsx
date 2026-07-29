import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import JourneyHome from '../pages/platform/journey/JourneyHome';
import { JourneyInvitations, JourneyOnboarding } from '../pages/platform/journey/JourneyBeginnings';
import { JourneySubplot } from '../pages/platform/journey/JourneyStories';
import { JourneyLive } from '../pages/platform/journey/JourneyConnections';

const renderPreview = (node: React.ReactNode, path: string) => render(
  <MemoryRouter initialEntries={[`${path}?design=preview`]}>{node}</MemoryRouter>,
);

describe('我的旅程设计预览', () => {
  // ⚠️ 九 → 八：「为凯恩·夜誓封卷」（遗作馆）随 memorial 整块删除（2026-07-29）。
  // 产品模型改为角色卡永不损失，「封卷成只读传世卡」这件事不存在了。见 VALIDATION §3.61。
  it('首页提供八个完整功能入口', () => {
    renderPreview(<JourneyHome />, '/platform/journey');
    expect(screen.getAllByText('从第一段旅程开始').length).toBeGreaterThan(0);
    expect(screen.getByText('房间邀请')).toBeInTheDocument();
    expect(screen.getByText('角色解释权')).toBeInTheDocument();
    expect(screen.getByText('开启私人平行线')).toBeInTheDocument();
    expect(screen.getByText('我的副本卡')).toBeInTheDocument();
    expect(screen.getByText('解锁真实身份')).toBeInTheDocument();
    expect(screen.getByText('欢迎回到故事里')).toBeInTheDocument();
    expect(screen.getByText('今夜开演')).toBeInTheDocument();
  });

  it('开场礼领取后才出现微世界启动动作', async () => {
    renderPreview(<JourneyOnboarding />, '/platform/journey/onboarding');
    fireEvent.click(screen.getByRole('button', { name: /领取开场礼/ }));
    expect(await screen.findByRole('button', { name: /开启第一段微世界/ })).toBeInTheDocument();
    expect(screen.getByText('已领取')).toBeInTheDocument();
  });

  it('接受邀请后明确要求再完成世界入场', async () => {
    renderPreview(<JourneyInvitations />, '/platform/journey/invitations');
    fireEvent.click(screen.getAllByRole('button', { name: /接受邀请/ })[0]);
    expect(await screen.findByRole('button', { name: /前往世界并确认入场/ })).toBeInTheDocument();
    expect(screen.getAllByText('已接受').length).toBeGreaterThan(0);
  });

  it('同星副本卡可以在预览中完成合成', async () => {
    renderPreview(<JourneySubplot />, '/platform/journey/subplot');
    fireEvent.click(screen.getByRole('button', { name: /确认合成/ }));
    expect(await screen.findByText(/已合成：潮汐尽头的守望约定/)).toBeInTheDocument();
  });

  it('公开舞台可以发送并回显弹幕', async () => {
    renderPreview(<JourneyLive />, '/platform/journey/live');
    const input = screen.getByLabelText('公开弹幕');
    fireEvent.change(input, { target: { value: '守住那座灯塔。' } });
    fireEvent.keyDown(input, { key: 'Enter', code: 'Enter' });
    expect(await screen.findByText('守住那座灯塔。')).toBeInTheDocument();
  });
});

