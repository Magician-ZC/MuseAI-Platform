/**
 * 🔴 **人工校准面的「唯一写入路径」此前在后台是走不通的。**
 *
 * # 这里钉的是什么
 *
 * `admin_api/calibration.rs` 的六个端点恒下发 `editable: false` + 一句 `editPath`，
 * 内容是「阶段坐标只在建模板时录入：`POST /admin/world-templates` 的 `sagaId` + `stageNo`」。
 * 后台把这句话原样渲染给运营看——**而那个建模板表单里根本没有这两格**：
 * 2026-07-29 实测 `grep -rn "sagaId\|stageNo" admin/src` 零命中。
 *
 * 于是运营在校准页看得见「这个系列缺第 3 阶段」，却只能自己去 curl 那个端点。
 * 「只读 + 指一条走不通的路」比「只读」更糟：它看起来是有出口的。
 *
 * # 为什么补的是入口而不是写端点
 *
 * 模板是 **append-only** 的：改模板 = 建一条新行、新 id，admin 侧 `version` 恒为 1，
 * 两条模板之间**没有任何血缘字段**。给校准面加写端点会同时带出「血缘怎么表达」
 * 这个未决问题，且必然与既有写入面的校验链（`validate_skeleton_refs` 八段闸 + 成对性 + 范围）漂移成两份。
 * 所以这一轮做的是：**把那条唯一的路补通、并在校准页摆出路口**，六个端点仍然全只读。
 *
 * # 放这里的原因
 *
 * `admin/` 没有自己的 `npm test`（VALIDATION §3.47 A5），后台用例只有放进根
 * `src/__tests__/` 才会被 CI 的 frontend-test job 跑到。范式抄 `admin-slo-table.test.tsx`。
 */
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { MemoryRouter, Routes, Route } from 'react-router-dom';
import { beforeEach, describe, expect, it } from 'vitest';
import WorldsOps from '../../admin/src/pages/WorldsOps';
import {
  suggestedNextStageNo,
  newTemplateHref,
  templatesViewHref,
} from '../../admin/src/pages/Calibration';

/** 记录后台发出的每一个请求，供断言请求体形状。 */
let calls: { url: string; method: string; body: unknown }[] = [];

function stubFetch(routes: Record<string, unknown>) {
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    calls.push({
      url,
      method: init?.method ?? 'GET',
      body: init?.body ? JSON.parse(String(init.body)) : undefined,
    });
    const hit = Object.entries(routes).find(([p]) => url.includes(p));
    // 约定：没预期到的请求直接 500，而不是给个空对象糊过去——
    // 否则「页面偷偷发了个没人管的请求」这类缺陷会被静默放过。
    if (!hit) return new Response(JSON.stringify({ error: { code: 'unexpected', message: url } }), { status: 500 });
    return new Response(JSON.stringify(hit[1]), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  }) as typeof fetch;
}

beforeEach(() => {
  calls = [];
  sessionStorage.setItem('museai-admin-token', 'tok');
});

const renderAt = (path: string) =>
  render(
    <MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route path="/worlds" element={<WorldsOps />} />
      </Routes>
    </MemoryRouter>,
  );

describe('suggestedNextStageNo', () => {
  it('🔴 有缺号时先补缺号——缺号意味着玩家会撞上一段走不通的路，比「还没写到那儿」严重', () => {
    expect(suggestedNextStageNo({ missingStageNos: [3, 5], maxStageNo: 7 })).toBe(3);
  });

  it('反向配对：没有缺号时才往后接一阶', () => {
    expect(suggestedNextStageNo({ missingStageNos: [], maxStageNo: 7 })).toBe(8);
  });

  it('空系列从第 1 阶起——缺号口径本来就是从 1 起算，「缺开篇」也算缺', () => {
    expect(suggestedNextStageNo({ missingStageNos: [], maxStageNo: null })).toBe(1);
  });

  it('取最小缺号，不依赖服务端把数组排好序（那是它的实现细节，不是响应契约）', () => {
    expect(suggestedNextStageNo({ missingStageNos: [7, 2, 5], maxStageNo: 9 })).toBe(2);
  });

  it('脏数据（0 / 负数 / NaN）被滤掉，不会算出一个不存在的阶段号', () => {
    expect(suggestedNextStageNo({ missingStageNos: [0, -3, Number.NaN], maxStageNo: 4 })).toBe(5);
  });
});

describe('newTemplateHref / templatesViewHref', () => {
  it('把阶段坐标带到建模板表单，并保留既有 query', () => {
    const href = newTemplateHref('?design=preview&view=calibration', 'douluo', 3);
    const q = new URLSearchParams(href.split('?')[1]);
    expect(href.startsWith('/worlds?')).toBe(true);
    expect(q.get('view')).toBe('templates');
    expect(q.get('newTemplate')).toBe('1');
    expect(q.get('sagaId')).toBe('douluo');
    expect(q.get('stageNo')).toBe('3');
    expect(q.get('design')).toBe('preview'); // 既有 query 不能被吃掉
  });

  it('🔴「去建模板」必须清掉上一次的坐标——否则会弹出一个填着别人系列号的建模板框', () => {
    const href = templatesViewHref('?view=calibration&newTemplate=1&sagaId=other&stageNo=9');
    const q = new URLSearchParams(href.split('?')[1]);
    expect(q.get('view')).toBe('templates');
    expect(q.get('newTemplate')).toBeNull();
    expect(q.get('sagaId')).toBeNull();
    expect(q.get('stageNo')).toBeNull();
  });
});

describe('建模板表单的阶段坐标两格', () => {
  it('🔴 表单里有 sagaId 与 stageNo 两格——此前 admin/src 全域零命中', async () => {
    stubFetch({ '/admin/world-templates': { templates: [], nextCursor: null } });
    renderAt('/worlds?view=templates');
    fireEvent.click(await screen.findByText('新建模板'));
    expect(await screen.findByPlaceholderText('世界系列 ID，如 douluo')).toBeInTheDocument();
    expect(screen.getByPlaceholderText('阶段号 1-999')).toBeInTheDocument();
  });

  it('从校准页跳过来时预填坐标并直接弹开建模板框', async () => {
    stubFetch({ '/admin/world-templates': { templates: [], nextCursor: null } });
    renderAt('/worlds?view=templates&newTemplate=1&sagaId=douluo&stageNo=3');
    const saga = (await screen.findByPlaceholderText('世界系列 ID，如 douluo')) as HTMLInputElement;
    expect(saga.value).toBe('douluo');
    expect((screen.getByPlaceholderText('阶段号 1-999') as HTMLInputElement).value).toBe('3');
  });

  it('🔴 成对性：只填 sagaId 不填阶段号，前端就拦下来（服务端会 400，但那句话不说人话）', async () => {
    stubFetch({ '/admin/world-templates': { templates: [], nextCursor: null } });
    renderAt('/worlds?view=templates');
    fireEvent.click(await screen.findByText('新建模板'));
    fireEvent.change(screen.getByPlaceholderText('世界系列 ID，如 douluo'), { target: { value: 'douluo' } });
    // 标题与骨架都填好，确保拦下来的原因只可能是成对性。
    const dialog = screen.getByRole('dialog');
    fireEvent.change(dialog.querySelectorAll('input')[0], { target: { value: '斗罗·第一阶' } });
    fireEvent.change(screen.getByPlaceholderText('{ "hardNodes": [], "endings": [] }'), {
      target: { value: '{}' },
    });
    fireEvent.click(screen.getByText('创 建'));
    expect(await screen.findByText(/必须成对填写/)).toBeInTheDocument();
    expect(calls.some((c) => c.method === 'POST')).toBe(false);
  });

  it('两格都留空时请求体里压根不出现这两个键——独立模板的行为与接线前逐字相同', async () => {
    stubFetch({ '/admin/world-templates': { templates: [], nextCursor: null, templateId: 'tpl_1', moderation: 'pending' } });
    renderAt('/worlds?view=templates');
    fireEvent.click(await screen.findByText('新建模板'));
    const dialog = screen.getByRole('dialog');
    fireEvent.change(dialog.querySelectorAll('input')[0], { target: { value: '独立模板' } });
    fireEvent.change(screen.getByPlaceholderText('{ "hardNodes": [], "endings": [] }'), {
      target: { value: '{}' },
    });
    fireEvent.click(screen.getByText('创 建'));
    await waitFor(() => expect(calls.some((c) => c.method === 'POST')).toBe(true));
    const post = calls.find((c) => c.method === 'POST')!;
    expect(post.body).not.toHaveProperty('sagaId');
    expect(post.body).not.toHaveProperty('stageNo');
  });

  it('两格都填好时，坐标随建模板请求一并下发', async () => {
    stubFetch({ '/admin/world-templates': { templates: [], nextCursor: null, templateId: 'tpl_1', moderation: 'pending' } });
    renderAt('/worlds?view=templates&newTemplate=1&sagaId=douluo&stageNo=3');
    const dialog = await screen.findByRole('dialog');
    fireEvent.change(dialog.querySelectorAll('input')[0], { target: { value: '斗罗·第三阶' } });
    fireEvent.change(screen.getByPlaceholderText('{ "hardNodes": [], "endings": [] }'), {
      target: { value: '{}' },
    });
    fireEvent.click(screen.getByText('创 建'));
    await waitFor(() => expect(calls.some((c) => c.method === 'POST')).toBe(true));
    const post = calls.find((c) => c.method === 'POST')!;
    expect(post.body).toMatchObject({ sagaId: 'douluo', stageNo: 3 });
  });
});

describe('校准页的路口', () => {
  it('阶段切分表给每个系列一个「补/录第 N 阶」的入口，且缺号优先', async () => {
    stubFetch({
      '/admin/sagas': {
        sagas: [
          {
            sagaId: 'douluo',
            templateCount: 4,
            stageCount: 4,
            minStageNo: 1,
            maxStageNo: 5,
            missingStageNos: [3],
            missingStageNosTruncated: false,
            duplicateStageNos: [],
            unnumberedTemplateCount: 0,
            contiguous: false,
            moderationCounts: { approved: 4 },
            roomTypes: ['idle'],
            starMin: 1,
            starMax: 2,
            worldCount: 10,
            liveWorldCount: 2,
            lastCreatedAt: 1_700_000_000_000,
          },
        ],
        editable: false,
        editPath: '阶段坐标只在建模板时录入：POST /admin/world-templates 的 sagaId + stageNo',
        notes: [],
      },
    });
    renderAt('/worlds?view=calibration');
    expect(await screen.findByText('补第 3 阶')).toBeInTheDocument();
    // 只读横幅还在——补的是路口，不是把这一页变成可写。
    expect(screen.getByText('本页只可视化，不可编辑')).toBeInTheDocument();
  });
});
