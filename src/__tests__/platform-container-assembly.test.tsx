// 自定义房装配的玩家端入口（总规格 §10「自定义房闭环」的最后一步）。
//
// 本文件钉两类东西：
// ① **纯函数**（`buildContainerSkeleton` / `containerBlockers` / `cardUsable`）——
//    它们决定「提交给服务端的骨架长什么样」，是这块功能唯一会产生副作用的地方；
// ② **面板的可见性与候选面**——决定用户能不能选到一个必然被服务端拒绝的东西。
//
// 🔴 第一条红线是「不装卡时提交体逐字节不变」。它不是洁癖：
// `POST /assets/worlds` 的 skeletonJson 会进超集校验、进机审、进 world_templates 落库，
// 且服务端的容器逻辑以 `Skeleton.container.is_none()` 为唯一开关点。
// 前端若在不装卡时也顺手重排/补键，整条「默认路径零变化」的保证就从类型层面退化成人读注释。
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';

vi.mock('../utils/cloudApi', () => {
  // 真实形态 CloudError：面板按 `instanceof CloudError` + status 区分「404 静默隐藏」与「其它错误提示」，
  // 给个普通 Error 会静默走到隐藏分支，用例看起来还是绿的。
  class CloudError extends Error {
    constructor(public code: string, message: string, public status: number) {
      super(message);
    }
  }
  return {
    cloudFetch: vi.fn(),
    cloudStream: vi.fn(() => () => {}),
    getPlatformBase: vi.fn(() => 'http://test'),
    setPlatformBase: vi.fn(),
    CloudError,
  };
});

import { cloudFetch, CloudError } from '../utils/cloudApi';
import ContainerAssemblyPanel, {
  anchorCandidates,
  buildContainerSkeleton,
  containerBlockers,
  cardUsable,
  draftLocations,
  EMPTY_SELECTION,
  NEXUS_RESERVED_ID,
  type ContainerSelection,
  type SubplotCardSummary,
} from '../pages/platform/ContainerAssembly';
import type { WorldSkeletonDraft } from '../stores/useWorldExtractionStore';

const fetchMock = cloudFetch as unknown as Mock;

beforeEach(() => {
  fetchMock.mockReset();
});

/** 一份典型的合成产物：两个地点，其中一个是秘境。 */
const draft = (): WorldSkeletonDraft => ({
  isSuperset: true,
  sourceWork: { sourceId: 'src-1', title: '雾海纪' },
  locations: [
    { id: 'core-hub', connections: [] },
    { id: 'secret-vault', isSecretRealm: true, connections: [] },
  ],
  worldItems: [{ id: 'wi-1' }],
  mainlineNodes: [{ id: 'mn-1' }],
  storylines: [{ id: 'sl-1' }],
});

const ownedCard = (over: Partial<SubplotCardSummary> = {}): SubplotCardSummary => ({
  id: 'scard_a',
  starRating: 2,
  label: '未寄出的潮汐信',
  originKind: 'settlement',
  status: 'owned',
  source: { worldId: 'w1', templateId: 'tpl_a', templateVersion: 3 },
  ...over,
});

const renderPanel = (
  value: ContainerSelection = EMPTY_SELECTION,
  onChange: (n: ContainerSelection) => void = () => {},
  d: WorldSkeletonDraft = draft(),
) => render(<ContainerAssemblyPanel draft={d} value={value} onChange={onChange} />);

describe('buildContainerSkeleton', () => {
  it('🔴 一张卡都没装时，返回的就是原对象本身（引用相等，不是浅拷贝）', () => {
    const d = draft();
    expect(buildContainerSkeleton(d, EMPTY_SELECTION)).toBe(d);
    // 只填了锚点/枢纽名但没选卡，同样不得改变提交体——那些键脱离 subplotCardRefs 毫无意义，
    // 单独发出去只会在服务端的「无人读取的键」那道闸上撞成 400。
    expect(buildContainerSkeleton(d, { cards: [], anchors: ['core-hub'], nexusName: '十字驿站' })).toBe(d);
  });

  it('装卡后只**附加**容器声明键，既有键一个都不动', () => {
    const d = draft();
    const out = buildContainerSkeleton(d, {
      cards: [{ cardId: 'scard_a', cardVersion: 3, weight: 1 }],
      anchors: [],
      nexusName: '',
    });
    expect(out).not.toBe(d);
    expect(out.subplotCardRefs).toEqual([{ cardId: 'scard_a', cardVersion: 3, weight: 1 }]);
    // 既有键逐个原样保留（含嵌套对象的引用）。
    for (const k of Object.keys(d)) {
      expect(out[k]).toBe(d[k]);
    }
    // 没选锚点/没填枢纽名 → 这两个键压根不出现（而不是出现一个空值）。
    expect('anchors' in out).toBe(false);
    expect('nexus' in out).toBe(false);
  });

  it('锚点与枢纽名只在真的填了时才出现，且去空白', () => {
    const out = buildContainerSkeleton(draft(), {
      cards: [{ cardId: 'scard_a', cardVersion: 3, weight: 0.5 }],
      anchors: ['  core-hub  ', '   ', ''],
      nexusName: '  十字驿站  ',
    });
    expect(out.anchors).toEqual(['core-hub']);
    expect(out.nexus).toEqual({ name: '十字驿站' });
  });

  it('拿不到蓝图版本的卡不写 cardVersion 键（而不是写一个 null 让服务端去猜）', () => {
    const out = buildContainerSkeleton(draft(), {
      cards: [{ cardId: 'scard_a', weight: 1 }],
      anchors: [],
      nexusName: '',
    });
    expect(out.subplotCardRefs).toEqual([{ cardId: 'scard_a', weight: 1 }]);
  });

  it('空 cardId 被丢弃；全被丢光时退回「没装卡」那条路径', () => {
    const d = draft();
    expect(buildContainerSkeleton(d, { cards: [{ cardId: '   ', weight: 1 }], anchors: [], nexusName: '' })).toBe(d);
  });
});

describe('containerBlockers / cardUsable / draftLocations', () => {
  it('本体占用了枢纽保留 id → 报出来（服务端必拒，让用户白填一遍是最差的选择）', () => {
    const d = draft();
    d.locations = [{ id: NEXUS_RESERVED_ID, connections: [] }];
    expect(containerBlockers(d).join()).toContain(NEXUS_RESERVED_ID);
  });

  it('本体地点 id 含命名空间分隔符 → 报出来', () => {
    const d = draft();
    d.locations = [{ id: 'evil:hub', connections: [] }];
    expect(containerBlockers(d).join()).toContain('evil:hub');
  });

  it('反向配对：正常的合成产物一条阻断都没有', () => {
    expect(containerBlockers(draft())).toEqual([]);
  });

  it('只有 owned 且带完整内容蓝图坐标的卡可装', () => {
    expect(cardUsable(ownedCard())).toBe(true);
    expect(cardUsable(ownedCard({ status: 'consumed' }))).toBe(false);
    expect(cardUsable(ownedCard({ source: { templateId: null, templateVersion: null } }))).toBe(false);
    expect(cardUsable(ownedCard({ source: { templateId: 'tpl_a', templateVersion: null } }))).toBe(false);
    expect(cardUsable(ownedCard({ source: undefined }))).toBe(false);
  });

  it('地点收窄能扛住脏数据（非对象 / 无 id / id 为空白）', () => {
    const d = draft();
    d.locations = [null, 'nope', { connections: [] }, { id: '   ' }, { id: 'ok' }];
    expect(draftLocations(d)).toEqual([{ id: 'ok', isSecretRealm: false }]);
  });

  it('🔴 秘境不进锚点候选——gate 语义必须完整留在它自己那一片', () => {
    expect(anchorCandidates(draft()).map((l) => l.id)).toEqual(['core-hub']);
  });

  it('反向配对：把秘境标记去掉，它就该出现在候选里（这条闸拦的是秘境，不是「地点」）', () => {
    const d = draft();
    d.locations = [
      { id: 'core-hub', connections: [] },
      { id: 'secret-vault', isSecretRealm: false, connections: [] },
    ];
    expect(anchorCandidates(d).map((l) => l.id)).toEqual(['core-hub', 'secret-vault']);
  });
});

describe('ContainerAssemblyPanel', () => {
  it('🔴 一张可装的卡都没有时整段不渲染——「功能没开」与「你没有卡」刻意不可区分', async () => {
    fetchMock.mockResolvedValue({ cards: [] });
    const { container } = renderPanel();
    await waitFor(() => expect(fetchMock).toHaveBeenCalledWith('/api/me/subplot-cards?status=owned'));
    expect(container).toBeEmptyDOMElement();
  });

  it('🔴 端点 404（功能未开放）同样静默隐藏，不提示「该功能未开放」', async () => {
    fetchMock.mockRejectedValue(new CloudError('not_found', 'HTTP 404', 404));
    const { container } = renderPanel();
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    expect(container).toBeEmptyDOMElement();
  });

  it('非 404 的读取失败给一句提示，但不挡住世界发布', async () => {
    fetchMock.mockRejectedValue(new CloudError('internal', '服务器开小差', 500));
    renderPanel();
    expect(await screen.findByText(/副本卡读取失败/)).toBeInTheDocument();
  });

  it('有卡时列出可装的卡，勾选后把 cardVersion 一并钉进声明', async () => {
    fetchMock.mockResolvedValue({ cards: [ownedCard()] });
    const onChange = vi.fn();
    renderPanel(EMPTY_SELECTION, onChange);
    const box = await screen.findByLabelText('装入副本卡 未寄出的潮汐信');
    fireEvent.click(box);
    expect(onChange).toHaveBeenCalledWith({
      cards: [{ cardId: 'scard_a', cardVersion: 3, weight: 1 }],
      anchors: [],
      nexusName: '',
    });
  });

  it('没有内容蓝图的卡不进候选表——它在服务端装配时必然 400', async () => {
    fetchMock.mockResolvedValue({
      cards: [ownedCard({ id: 'scard_bad', label: '无蓝图的卡', source: { templateId: null, templateVersion: null } })],
    });
    const { container } = renderPanel();
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    expect(container).toBeEmptyDOMElement();
  });

  it('本体占用枢纽保留 id 时整段禁用并说明，不给用户白填的机会', async () => {
    fetchMock.mockResolvedValue({ cards: [ownedCard()] });
    const d = draft();
    d.locations = [{ id: NEXUS_RESERVED_ID, connections: [] }];
    renderPanel(EMPTY_SELECTION, () => {}, d);
    expect(await screen.findByText('这份世界超集暂时不能装卡')).toBeInTheDocument();
    expect(screen.queryByLabelText('装入副本卡 未寄出的潮汐信')).not.toBeInTheDocument();
  });

  it('权重 0 给警告：装配会成功，但那张卡的剧情线永远抽不中', async () => {
    fetchMock.mockResolvedValue({ cards: [ownedCard()] });
    renderPanel({ cards: [{ cardId: 'scard_a', cardVersion: 3, weight: 0 }], anchors: [], nexusName: '' });
    expect(await screen.findByText(/权重为 0 的卡/)).toBeInTheDocument();
  });

  it('反向配对：权重 1 时不出现那条警告', async () => {
    fetchMock.mockResolvedValue({ cards: [ownedCard()] });
    renderPanel({ cards: [{ cardId: 'scard_a', cardVersion: 3, weight: 1 }], anchors: [], nexusName: '' });
    await screen.findByLabelText('装入副本卡 未寄出的潮汐信');
    expect(screen.queryByText(/权重为 0 的卡/)).not.toBeInTheDocument();
  });

  it('选了卡之后才出现锚点与枢纽名两个可选项', async () => {
    fetchMock.mockResolvedValue({ cards: [ownedCard()] });
    renderPanel({ cards: [{ cardId: 'scard_a', cardVersion: 3, weight: 1 }], anchors: [], nexusName: '' });
    expect(await screen.findByLabelText('对外缝合口')).toBeInTheDocument();
    expect(screen.getByLabelText('枢纽地点名')).toBeInTheDocument();
  });

  it('没选卡时不显示锚点/枢纽名——它们脱离 subplotCardRefs 没有意义', async () => {
    fetchMock.mockResolvedValue({ cards: [ownedCard()] });
    renderPanel();
    await screen.findByLabelText('装入副本卡 未寄出的潮汐信');
    expect(screen.queryByLabelText('对外缝合口')).not.toBeInTheDocument();
    expect(screen.queryByLabelText('枢纽地点名')).not.toBeInTheDocument();
  });
});
