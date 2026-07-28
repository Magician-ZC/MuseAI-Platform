/**
 * 🔴 **后台 SLO 表的渲染契约：`status ≠ ok` 一律不画数字。**
 *
 * # 为什么这个文件在主前端的 `src/__tests__/` 里
 *
 * `admin/` **没有自己的测试基建**（只有 `tsc`，见 `docs/VALIDATION.md` §3.47 欠账 A5）。
 * 登记那条欠账时我写的是「引入 vitest 要加依赖，属决定不是实现」——**那句话没走一遍就写了**：
 * 根目录的 devDependencies 里 vitest / jsdom / @testing-library/react 全都有，
 * 而 `admin` 的 7 个依赖（antd、echarts、react-router-dom…）**根目录一个都不缺**。
 * 于是后台组件可以直接跑在根 vitest 下，**一个新依赖都不用加**。
 *
 * ⚠️ 这不等于 A5 全解：admin 仍然没有**自己的** `npm test`，
 * 新写的后台测试得记得放到这里。但「后台代码零测试覆盖」这个实际风险已经不成立了。
 *
 * # 这里钉的是什么
 *
 * §3.36 定下的规矩：显示 `—` 与显示 `0%` 是两个完全不同的经营判断。
 * 服务端那一半有红线守着（`empty_database_says_no_data_rather_than_zero`），
 * 前端这一半此前**一道门都没有**——而前端才是运营真正看到的那一层。
 */
import { render, screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import Metrics from '../../admin/src/pages/Metrics';

/** 让被测组件拿到一份可控的 overview，不碰真实网络。 */
function mockOverview(narrativeSlo: unknown) {
  const payload = {
    users: { total: 0, banned: 0 },
    dailyReports: { total: 0, opened: 0, openRate: 0 },
    ticks: { total: 0, done: 0, failed: 0, successRate: 0 },
    tokenCostByWorld: [],
    auditBacklog: 0,
    worlds: { active: 0, fused: 0 },
    riskEvents: 0,
    dataRequestsPending: 0,
    narrativeSlo,
  };
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = String(input);
    const body = url.includes('/metrics/trends') ? [] : payload;
    return new Response(JSON.stringify(body), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  }) as typeof fetch;
}

const OK_BLOCK = {
  metric: 'attentionGini',
  title: '叙事注意力基尼系数',
  status: 'ok',
  overThresholdRate: 0.25,
  worldsCounted: 8,
  threshold: 0.35,
};
const NO_DATA_BLOCK = {
  metric: 'forcedConclusionRate',
  title: '强制收尾率',
  status: 'no_data_yet',
  forcedRate: null,
  endedWorlds: 0,
  notes: ['一个世界都还没结束过'],
};

describe('后台 SLO 表', () => {
  it('🔴 status ≠ ok 的指标一律不画数字，只出 — 与中文状态', async () => {
    mockOverview({ status: 'ok', windowDays: 7, metrics: { forcedConclusionRate: NO_DATA_BLOCK } });
    render(<Metrics />);

    const row = (await screen.findByText('强制收尾率')).closest('tr');
    expect(row).not.toBeNull();
    const cells = within(row as HTMLElement);
    // 读数列与门槛列都会是 —，故用 getAllByText。
    expect(cells.getAllByText('—').length).toBeGreaterThan(0);
    expect(cells.getByText('至今零样本')).toBeInTheDocument();
    // 🔴 这一行里绝不许出现任何百分数——0% 与 — 是两句完全不同的话。
    expect((row as HTMLElement).textContent ?? '').not.toMatch(/\d+(\.\d+)?%/);
  });

  it('反向配对：status = ok 的指标必须**照常画出数字**', async () => {
    mockOverview({ status: 'ok', windowDays: 7, metrics: { attentionGini: OK_BLOCK } });
    render(<Metrics />);

    const row = (await screen.findByText('叙事注意力基尼系数')).closest('tr');
    expect(row).not.toBeNull();
    // 只测「非 ok 不画数」的话，把整张表恒画 — 也能全绿——那等于把这段废掉。
    expect((row as HTMLElement).textContent ?? '').toMatch(/25\.0%/);
    expect(within(row as HTMLElement).getByText(/8 个世界/)).toBeInTheDocument();
  });

  it('服务端上线的新指标不会被静默漏掉（前端未登记时按通用形态显示）', async () => {
    mockOverview({
      status: 'ok',
      windowDays: 7,
      metrics: { brandNewMetric: { metric: 'brandNewMetric', title: '某个新指标', status: 'ok', value: 0.5 } },
    });
    render(<Metrics />);
    expect(await screen.findByText('某个新指标')).toBeInTheDocument();
    expect(await screen.findByText('新指标')).toBeInTheDocument();
  });
});
