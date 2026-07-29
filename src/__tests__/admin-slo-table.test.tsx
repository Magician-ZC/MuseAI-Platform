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
 * ⚠️ 原先这里写着「admin 仍然没有**自己的** `npm test`」——**2026-07-29 起那句话过期了**：
 * `admin/package.json` 有了 `"test": "npm --prefix .. run test -- --run src/__tests__/admin-"`，
 * `cd admin && npm test` 现在真的能跑（零新依赖，它转身去调根仓库的 vitest）。
 * 🔴 但要知道它是**一个便利别名，不是一道门**：过滤靠的是 `admin-` 这个文件名前缀，
 * 而这条命名约定没有任何东西守着——新写的后台用例只要没起这个前缀，就静默不进这个命令
 * （根 `npm run test` 仍会跑到，所以 CI 不会红，这恰恰让人发现不了）。写在这里，供下一个人核对。
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
