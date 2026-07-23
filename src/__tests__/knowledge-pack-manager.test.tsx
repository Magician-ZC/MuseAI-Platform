import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, type Mock } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import KnowledgePackManager from '../components/KnowledgePackManager';
import { useKnowledgePackStore, type KnowledgePack } from '../stores/useKnowledgePackStore';
import { usePartnerStore } from '../stores/usePartnerStore';

const mockInvoke = invoke as unknown as Mock;

const pack = (): KnowledgePack => ({
  schemaVersion: 1,
  id: 'p1',
  title: '孙子兵法',
  source: {
    path: '/src/art-of-war.txt',
    contentHash: 'h',
    rightsBasis: 'public_domain',
    allowedUses: ['extract', 'retrieve'],
    retention: 'index_only',
  },
  mode: 'knowledge',
  distilled: { principles: [] },
  chunkIndexStoreKey: 'knowledge/index/p1.json',
  indexVersion: 'iv1',
  revision: 1,
});

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (cmd: string) => {
    if (cmd === 'list_knowledge_packs') return [pack()];
    if (cmd === 'list_knowledge_bindings') return [];
    if (cmd === 'import_knowledge_source')
      return { pack: pack(), chunkStats: { chunkCount: 12, totalChars: 5000 } };
    if (cmd === 'search_knowledge')
      return [{ packId: 'p1', packTitle: '孙子兵法', chunkId: 'ch1', ordinal: 1, text: '兵者，诡道也。', score: 0.92 }];
    if (cmd === 'upsert_knowledge_binding') return undefined;
    return undefined;
  });
  useKnowledgePackStore.setState({ packs: [], bindings: [], fragments: [], selectedPackIds: [], searchLimit: 5 });
  usePartnerStore.setState({ characterCardsV2: [], characterCards: [] });
});

describe('KnowledgePackManager', () => {
  it('打开即拉取包列表并渲染三个标签页', async () => {
    render(<KnowledgePackManager open onClose={() => {}} />);

    expect(screen.getByText('知识包')).toBeInTheDocument();
    expect(screen.getByText('检索预览')).toBeInTheDocument();
    expect(screen.getByText('角色绑定')).toBeInTheDocument();

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith('list_knowledge_packs', undefined);
    });
    expect(await screen.findByText('孙子兵法')).toBeInTheDocument();
  });

  it('导入知识源：填写权利基础与用途后调用 import_knowledge_source', async () => {
    render(<KnowledgePackManager open onClose={() => {}} />);

    fireEvent.change(screen.getByLabelText('知识包标题'), { target: { value: '战争论' } });
    fireEvent.change(screen.getByLabelText('源文件路径'), { target: { value: '/src/on-war.txt' } });
    fireEvent.click(screen.getByRole('button', { name: /导入/ }));

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith(
        'import_knowledge_source',
        expect.objectContaining({
          request: expect.objectContaining({ title: '战争论', sourcePath: '/src/on-war.txt' }),
        }),
      );
    });
  });

  it('角色绑定：启停对照切换调用 upsert_knowledge_binding', async () => {
    useKnowledgePackStore.setState({
      packs: [pack()],
      bindings: [
        {
          id: 'b1',
          packId: 'p1',
          characterId: 'c1',
          influence: 0.6,
          enabled: true,
          conflictPolicy: 'character_core_wins',
        },
      ],
    });
    render(<KnowledgePackManager open onClose={() => {}} />);

    fireEvent.click(screen.getByText('角色绑定'));
    // 启停对照开关（enabled: true → 关闭）
    fireEvent.click(screen.getByRole('switch', { name: '启停 b1' }));

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith(
        'upsert_knowledge_binding',
        expect.objectContaining({ binding: expect.objectContaining({ id: 'b1', enabled: false }) }),
      );
    });
  });

  it('检索预览：展示已返回的知识片段', () => {
    useKnowledgePackStore.setState({
      packs: [pack()],
      fragments: [{ packId: 'p1', packTitle: '孙子兵法', chunkId: 'ch1', ordinal: 1, text: '兵者，诡道也。', score: 0.92 }],
    });
    render(<KnowledgePackManager open onClose={() => {}} />);
    fireEvent.click(screen.getByText('检索预览'));
    expect(screen.getByText('兵者，诡道也。')).toBeInTheDocument();
  });
});
