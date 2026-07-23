import { describe, expect, it } from 'vitest';
import {
  parseOutline,
  serializeOutline,
  validateForbiddenExpression,
  type OutlineNode,
} from '../utils/storyConstraints';

describe('parseOutline', () => {
  it('解析三种前缀 + 缺省软 + 忽略空行', () => {
    const text = [
      '[硬] 抵达雪山之巅',
      '[软] 与向导发生争执',
      '',
      '翻越冰川',
      '   ',
      '[自由] 自由发挥的支线',
    ].join('\n');

    const nodes = parseOutline(text);
    expect(nodes).toHaveLength(4);
    expect(nodes.map((n) => n.constraint)).toEqual(['hard', 'soft', 'soft', 'free']);
    expect(nodes.map((n) => n.summary)).toEqual([
      '抵达雪山之巅',
      '与向导发生争执',
      '翻越冰川',
      '自由发挥的支线',
    ]);
    expect(nodes.every((n) => n.status === 'pending')).toBe(true);
    expect(nodes.map((n) => n.id)).toEqual(['node-1', 'node-2', 'node-3', 'node-4']);
  });

  it('容忍全角括号与前缀内空白', () => {
    const nodes = parseOutline('【硬】 决战\n[ 自由 ]支线');
    expect(nodes[0].constraint).toBe('hard');
    expect(nodes[0].summary).toBe('决战');
    expect(nodes[1].constraint).toBe('free');
    expect(nodes[1].summary).toBe('支线');
  });

  it('空文本得到空数组', () => {
    expect(parseOutline('')).toEqual([]);
    expect(parseOutline('\n\n  \n')).toEqual([]);
  });

  it('支持自定义 id 前缀', () => {
    const nodes = parseOutline('A\nB', { idPrefix: 'outline' });
    expect(nodes.map((n) => n.id)).toEqual(['outline-1', 'outline-2']);
  });
});

describe('serializeOutline', () => {
  it('往返稳定（默认软节点省略前缀）', () => {
    const text = '[硬] 抵达雪山之巅\n与向导发生争执\n[自由] 自由支线';
    const nodes = parseOutline(text);
    const serialized = serializeOutline(nodes);
    expect(serialized).toBe(text);

    const round = parseOutline(serialized);
    expect(round.map((n) => n.constraint)).toEqual(nodes.map((n) => n.constraint));
    expect(round.map((n) => n.summary)).toEqual(nodes.map((n) => n.summary));
  });

  it('explicitSoft 时软节点也带前缀', () => {
    const nodes: OutlineNode[] = [
      { id: 'node-1', summary: '争执', constraint: 'soft', status: 'pending' },
    ];
    expect(serializeOutline(nodes)).toBe('争执');
    expect(serializeOutline(nodes, { explicitSoft: true })).toBe('[软] 争执');
  });
});

describe('validateForbiddenExpression', () => {
  it.each([
    ['characters.li.secrets contains "身世"', 'contains'],
    ['characters.li-xiaoyao.goals contains "复仇"', 'contains'],
    ['characters.li.arcStage == "觉醒"', 'arcStage'],
    ['world.day == 3', 'world'],
    ['world.weather == "storm"', 'world'],
    ['world.config == {"phase":2}', 'world'],
    ['world.flag == true', 'world'],
    ['relations[li->wang].trust > 0.5', 'relation'],
    ['relations[a->b].fear == 2', 'relation'],
    ['relations[a->b].debt < -1.5', 'relation'],
  ])('合法：%s', (expr, form) => {
    const result = validateForbiddenExpression(expr);
    expect(result.valid).toBe(true);
    expect(result.form).toBe(form);
  });

  it.each([
    [''],
    ['characters.li.secrets 包含 "x"'],
    ['characters.li.arcStage = "觉醒"'],
    ['world.day =='],
    ['world.day == {bad json'],
    ['relations[li->wang].trust >= 0.5'],
    ['relations[li->wang].trust > abc'],
    ['random.stuff contains "x"'],
    ['just some prose'],
  ])('非法：%s', (expr) => {
    const result = validateForbiddenExpression(expr);
    expect(result.valid).toBe(false);
    expect(result.error).toBeTruthy();
    expect(result.form).toBeUndefined();
  });
});
