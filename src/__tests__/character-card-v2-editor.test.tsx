import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import CharacterCardV2Editor from '../components/CharacterCardV2Editor';
import { createEmptyCardV2, type CharacterCardV2, type EvidenceRef } from '../utils/characterCardV2';

const mockInvoke = invoke as unknown as Mock;

// 造一张关键行为字段齐全的卡（用于差异测试对照）。
const filledCard = (name: string): CharacterCardV2 => {
  const card = createEmptyCardV2(name);
  card.dramaticCore.coreContradiction = '忠诚与自保';
  card.dramaticCore.surfaceGoal = '守住城池';
  card.dramaticCore.coreFear = '背叛';
  card.dramaticCore.stakes = '全族存亡';
  card.decisionModel.valuePriorities = ['家族', '荣誉'];
  card.decisionModel.decisionRules = [{ when: '受威胁', then: '先示弱', because: '保存实力' }];
  card.agency.plotSeeds = ['隐藏的血仇'];
  return card;
};

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
});

describe('CharacterCardV2Editor', () => {
  it('渲染十层结构与校验状态条', () => {
    render(<CharacterCardV2Editor card={createEmptyCardV2('林逸')} />);

    expect(screen.getByText('A · 基础身份')).toBeInTheDocument();
    expect(screen.getByText('B · 戏剧内核')).toBeInTheDocument();
    expect(screen.getByText('C · 决策模型')).toBeInTheDocument();
    expect(screen.getByText('J · 跨世界适配')).toBeInTheDocument();
    // 空卡：可达生命周期为草稿，提示待补充关键字段。
    expect(screen.getByText(/待补充/)).toBeInTheDocument();
  });

  it('编辑字段回调 onChange', () => {
    const onChange = vi.fn();
    render(<CharacterCardV2Editor card={createEmptyCardV2('林逸')} onChange={onChange} />);

    fireEvent.change(screen.getByLabelText('姓名'), { target: { value: '沈霜' } });
    expect(onChange).toHaveBeenCalled();
    const calls = onChange.mock.calls;
    const last = calls[calls.length - 1][0] as CharacterCardV2;
    expect(last.identity.name).toBe('沈霜');
  });

  it('展示证据溯源（引用到 decisionRules 的证据）', () => {
    const card = filledCard('林逸');
    card.decisionModel.decisionRules[0].evidenceIds = ['ev-1'];
    const evidence: EvidenceRef[] = [
      {
        id: 'ev-1',
        sourceId: 's1',
        chapterIndex: 3,
        locator: { start: 0, end: 10 },
        quotePreview: '他默默退了半步。',
        kind: 'action',
        confidence: 'high',
      },
    ];
    render(<CharacterCardV2Editor card={card} evidence={evidence} />);
    expect(screen.getByText('他默默退了半步。')).toBeInTheDocument();
    expect(screen.getByText('第 3 章')).toBeInTheDocument();
  });

  it('互换测试：同卡内容短路，不调用模型直接判定可互换', async () => {
    const a = createEmptyCardV2('甲');
    const b = createEmptyCardV2('甲'); // 内容一致（空卡），仅 id 不同
    render(<CharacterCardV2Editor card={a} otherCards={[b]} />);

    fireEvent.mouseDown(screen.getByRole('combobox', { name: '互换测试对象' }));
    fireEvent.click(await screen.findByText('甲'));
    fireEvent.click(screen.getByRole('button', { name: /运行互换测试/ }));

    expect(await screen.findByText(/两角色可互换/)).toBeInTheDocument();
    expect(mockInvoke).not.toHaveBeenCalledWith('run_character_swap_test', expect.anything());
  });

  it('互换测试：不同卡调用 run_character_swap_test 并展示报告', async () => {
    const a = filledCard('甲');
    const b = filledCard('乙');
    b.dramaticCore.coreContradiction = '野心与孤独'; // 制造内容差异
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'run_character_swap_test'
        ? {
            cardA: a.id,
            cardB: b.id,
            scenario: '盟友背叛',
            findings: [{ dimension: '首要选择', aBehavior: '死守', bBehavior: '反噬', distinct: true }],
            interchangeable: false,
            summary: '两人在同一局势下选择不同。',
          }
        : undefined,
    );
    render(<CharacterCardV2Editor card={a} otherCards={[b]} />);

    fireEvent.mouseDown(screen.getByRole('combobox', { name: '互换测试对象' }));
    fireEvent.click(await screen.findByText('乙'));
    fireEvent.change(screen.getByLabelText('互换测试局势'), { target: { value: '盟友背叛' } });
    fireEvent.click(screen.getByRole('button', { name: /运行互换测试/ }));

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith(
        'run_character_swap_test',
        expect.objectContaining({ scenario: '盟友背叛' }),
      );
    });
    expect(await screen.findByText(/两角色不可互换/)).toBeInTheDocument();
  });
});
