import { describe, expect, it } from 'vitest';
import {
  ARTICLE_TYPE_OPTIONS,
  buildRequiredSkillInstruction,
  getOutlineSkillName,
  getWriterSkillName,
} from '../constants/articleTypes';

const getMaleLongFormGenres = () => {
  const male = ARTICLE_TYPE_OPTIONS.find((option) => option.value === '男频');
  const longForm = male?.children?.find((option) => option.value === '长篇');
  return longForm?.children?.map((option) => option.value) ?? [];
};

describe('文章类型配置', () => {
  it('提供全部男频长篇题材', () => {
    expect(getMaleLongFormGenres()).toEqual([
      '玄幻脑洞',
      '西方玄幻',
      '东方仙侠',
      '历史古代',
      '游戏体育',
    ]);
  });
});

describe('文章类型 Skill 路由', () => {
  it.each([
    [['男频', '长篇', '西方玄幻'], 'fanqie-long-xifang-writer', 'fanqie-long-xifang-outline'],
    [['男频', '长篇', '东方仙侠'], 'fanqie-long-xianxia-writer', 'fanqie-long-xianxia-outline'],
    [['男频', '长篇', '历史古代'], 'fanqie-long-lishi-writer', 'fanqie-long-lishi-outline'],
    [['男频', '长篇', '游戏体育'], 'fanqie-long-youxi-writer', 'fanqie-long-youxi-outline'],
  ])('为 %j 返回匹配的 writer 和 outline Skill', (articleType, writer, outline) => {
    expect(getWriterSkillName(articleType)).toBe(writer);
    expect(getOutlineSkillName(articleType)).toBe(outline);
  });

  it('保持既有 Skill 路由', () => {
    expect(getWriterSkillName(['男频', '长篇', '玄幻脑洞'])).toBe('fanqie-xuanhuan-writer');
    expect(getOutlineSkillName(['男频', '长篇', '玄幻脑洞'])).toBe('fanqie-xuanhuan-outline');
    expect(getWriterSkillName(['女频', '短篇', '大女主'])).toBe('fanqie-short-danvzhu-writer');
    expect(getOutlineSkillName(['女频', '短篇', '大女主'])).toBe('fanqie-short-danvzhu-outline');
    expect(getWriterSkillName(['公众号'])).toBe('kitt-writer');
    expect(getOutlineSkillName(['公众号'])).toBeNull();
  });

  it('不会跨角色返回不匹配的 Skill', () => {
    expect(getWriterSkillName(['男频', '长篇', '西方玄幻'])).not.toContain('outline');
    expect(getOutlineSkillName(['男频', '长篇', '西方玄幻'])).not.toContain('writer');
    expect(getWriterSkillName(['未知'])).toBeNull();
    expect(getOutlineSkillName(['未知'])).toBeNull();
  });
});

describe('首轮 Skill 指令', () => {
  it('将单个 Skill 作为 Slash Command 和强制调用指令嵌入原消息', () => {
    expect(buildRequiredSkillInstruction('开始写第一章', ['fanqie-long-xifang-writer'])).toBe(
      '/fanqie-long-xifang-writer\n' +
      '【系统指令】本轮必须优先调用以下技能：fanqie-long-xifang-writer\n\n' +
      '开始写第一章',
    );
  });

  it('没有匹配 Skill 时保持原消息', () => {
    expect(buildRequiredSkillInstruction('开始写第一章', [])).toBe('开始写第一章');
  });
});
