import { beforeEach, describe, expect, it, type Mock } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  useKnowledgePackStore,
  type KnowledgeBinding,
  type KnowledgePack,
} from '../stores/useKnowledgePackStore';

const mockInvoke = invoke as unknown as Mock;

const makePack = (id: string, overrides: Partial<KnowledgePack> = {}): KnowledgePack => ({
  schemaVersion: 1,
  id,
  title: `包-${id}`,
  source: {
    path: `/src/${id}.txt`,
    contentHash: 'h',
    rightsBasis: 'owned',
    allowedUses: ['extract', 'retrieve'],
    retention: 'index_only',
  },
  mode: 'knowledge',
  distilled: { principles: [] },
  chunkIndexStoreKey: `knowledge/index/${id}.json`,
  indexVersion: 'iv1',
  revision: 1,
  ...overrides,
});

const makeBinding = (id: string, overrides: Partial<KnowledgeBinding> = {}): KnowledgeBinding => ({
  id,
  packId: 'pack-1',
  characterId: 'char-1',
  influence: 0.5,
  enabled: true,
  conflictPolicy: 'character_core_wins',
  ...overrides,
});

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async () => undefined);
  useKnowledgePackStore.setState({
    packs: [],
    bindings: [],
    fragments: [],
    lastError: null,
    selectedPackIds: [],
    searchLimit: 5,
  });
});

describe('useKnowledgePackStore pack list & CRUD', () => {
  it('listPacks 拉取并写入包列表', async () => {
    const packs = [makePack('pack-1'), makePack('pack-2')];
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'list_knowledge_packs' ? packs : undefined,
    );

    const result = await useKnowledgePackStore.getState().listPacks();

    expect(result).toEqual(packs);
    expect(mockInvoke).toHaveBeenCalledWith('list_knowledge_packs', undefined);
    expect(useKnowledgePackStore.getState().packs).toEqual(packs);
  });

  it('importSource 用 camelCase 请求并把新包并入列表', async () => {
    const pack = makePack('pack-new');
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'import_knowledge_source' ? { pack, chunkStats: { chunkCount: 3, totalChars: 900 } } : undefined,
    );

    const request = {
      sourcePath: '/src/new.txt',
      title: '新知识',
      rightsBasis: 'personal_use' as const,
      allowedUses: ['extract', 'send_to_remote_model'] as const,
      retention: 'managed_copy' as const,
    };
    const response = await useKnowledgePackStore.getState().importSource({
      ...request,
      allowedUses: [...request.allowedUses],
    });

    expect(response.chunkStats.chunkCount).toBe(3);
    expect(mockInvoke).toHaveBeenCalledWith('import_knowledge_source', {
      request: { ...request, allowedUses: [...request.allowedUses] },
    });
    expect(useKnowledgePackStore.getState().packs).toEqual([pack]);
  });

  it('distill 按 id 更新（upsert）已有包', async () => {
    const original = makePack('pack-1', { mode: 'knowledge' });
    const distilled = makePack('pack-1', { mode: 'mind', revision: 2 });
    useKnowledgePackStore.setState({ packs: [original] });
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'distill_knowledge_pack' ? distilled : undefined,
    );

    const result = await useKnowledgePackStore.getState().distill({
      packId: 'pack-1',
      mode: 'mind',
      profile: { interface: 'OpenAI-compatible', baseUrl: 'u', apiKey: 'k', model: 'm' },
      promptsByMode: { mind: '蒸馏 prompt' },
    });

    expect(result.mode).toBe('mind');
    expect(useKnowledgePackStore.getState().packs).toHaveLength(1);
    expect(useKnowledgePackStore.getState().packs[0].revision).toBe(2);
  });

  it('search 写入片段并默认使用 searchLimit', async () => {
    const fragments = [
      { packId: 'pack-1', packTitle: '包', chunkId: 'c1', ordinal: 0, text: '片段', score: 0.9 },
    ];
    useKnowledgePackStore.setState({ searchLimit: 7 });
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'search_knowledge' ? fragments : undefined,
    );

    const result = await useKnowledgePackStore.getState().search(['pack-1'], '查询');

    expect(result).toEqual(fragments);
    expect(mockInvoke).toHaveBeenCalledWith('search_knowledge', {
      packIds: ['pack-1'],
      query: '查询',
      limit: 7,
    });
    expect(useKnowledgePackStore.getState().fragments).toEqual(fragments);
  });

  it('deletePack 级联移除包、相关绑定与选中项', async () => {
    useKnowledgePackStore.setState({
      packs: [makePack('pack-1'), makePack('pack-2')],
      bindings: [makeBinding('b1', { packId: 'pack-1' }), makeBinding('b2', { packId: 'pack-2' })],
      selectedPackIds: ['pack-1', 'pack-2'],
    });
    mockInvoke.mockImplementation(async () => undefined);

    await useKnowledgePackStore.getState().deletePack('pack-1');

    expect(mockInvoke).toHaveBeenCalledWith('delete_knowledge_pack', { packId: 'pack-1' });
    expect(useKnowledgePackStore.getState().packs.map((p) => p.id)).toEqual(['pack-2']);
    expect(useKnowledgePackStore.getState().bindings.map((b) => b.id)).toEqual(['b2']);
    expect(useKnowledgePackStore.getState().selectedPackIds).toEqual(['pack-2']);
  });
});

describe('useKnowledgePackStore binding CRUD', () => {
  it('listBindings / upsertBinding（增改）/ removeBinding', async () => {
    const listed = [makeBinding('b1')];
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'list_knowledge_bindings' ? listed : undefined,
    );

    await useKnowledgePackStore.getState().listBindings();
    expect(useKnowledgePackStore.getState().bindings).toEqual(listed);

    // 新增
    const created = makeBinding('b2', { influence: 0.3 });
    await useKnowledgePackStore.getState().upsertBinding(created);
    expect(mockInvoke).toHaveBeenCalledWith('upsert_knowledge_binding', { binding: created });
    expect(useKnowledgePackStore.getState().bindings.map((b) => b.id)).toEqual(['b1', 'b2']);

    // 修改（同 id 覆盖，不新增）
    const modified = makeBinding('b2', { influence: 0.9, enabled: false });
    await useKnowledgePackStore.getState().upsertBinding(modified);
    expect(useKnowledgePackStore.getState().bindings).toHaveLength(2);
    expect(useKnowledgePackStore.getState().bindings[1].influence).toBe(0.9);

    // 删除
    await useKnowledgePackStore.getState().removeBinding('b1');
    expect(mockInvoke).toHaveBeenCalledWith('remove_knowledge_binding', { bindingId: 'b1' });
    expect(useKnowledgePackStore.getState().bindings.map((b) => b.id)).toEqual(['b2']);
  });
});

describe('useKnowledgePackStore UI preferences', () => {
  it('setSelectedPackIds / togglePackSelected / setSearchLimit', () => {
    const store = useKnowledgePackStore.getState();
    store.setSelectedPackIds(['pack-1']);
    expect(useKnowledgePackStore.getState().selectedPackIds).toEqual(['pack-1']);

    store.togglePackSelected('pack-2');
    expect(useKnowledgePackStore.getState().selectedPackIds).toEqual(['pack-1', 'pack-2']);
    store.togglePackSelected('pack-1');
    expect(useKnowledgePackStore.getState().selectedPackIds).toEqual(['pack-2']);

    store.setSearchLimit(12);
    expect(useKnowledgePackStore.getState().searchLimit).toBe(12);
  });
});
