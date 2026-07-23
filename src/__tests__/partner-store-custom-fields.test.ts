import { describe, expect, it } from 'vitest';
import { compileItemToMarkdown, PartnerItemFields } from '../stores/usePartnerStore';

describe('compileItemToMarkdown with custom fields', () => {
  it('includes world book custom fields in markdown output', () => {
    const fields: PartnerItemFields = {
      theme: '魔法冒险',
      customFields: [
        { id: 'cf-1', moduleId: 'world_basic', label: '货币体系', value: '金币与以太晶石' },
        { id: 'cf-2', moduleId: 'world_core', label: '禁忌知识', value: '古神真名' },
      ],
    };

    const md = compileItemToMarkdown('测试世界', 'world_book', fields);

    expect(md).toContain('## 自定义补充设定');
    expect(md).toContain('### 基本世界设定');
    expect(md).toContain('- **货币体系**：金币与以太晶石');
    expect(md).toContain('### 核心世界观架构');
    expect(md).toContain('- **禁忌知识**：古神真名');
  });

  it('includes character card custom fields in markdown output', () => {
    const fields: PartnerItemFields = {
      age: '18岁',
      customFields: [
        { id: 'cf-3', moduleId: 'char_basic', label: '昵称', value: '小明' },
        { id: 'cf-4', moduleId: 'char_appearance', label: '疤痕', value: '左肩有一道旧伤' },
        { id: 'cf-5', moduleId: 'char_ability', label: '隐藏技能', value: '可以与动物对话' },
        { id: 'cf-6', moduleId: 'char_memory', label: '共同秘密', value: '一起发现了地下密室' },
      ],
    };

    const md = compileItemToMarkdown('测试角色', 'character_card', fields);

    expect(md).toContain('## 自定义补充设定');
    expect(md).toContain('### 基础与标签');
    expect(md).toContain('- **昵称**：小明');
    expect(md).toContain('### 外貌与性格');
    expect(md).toContain('- **疤痕**：左肩有一道旧伤');
    expect(md).toContain('### 能力与经历');
    expect(md).toContain('- **隐藏技能**：可以与动物对话');
    expect(md).toContain('### 角色记忆');
    expect(md).toContain('- **共同秘密**：一起发现了地下密室');
  });

  it('omits custom fields section when no custom fields exist', () => {
    const fields: PartnerItemFields = {
      theme: '科幻',
      era: '未来',
    };

    const md = compileItemToMarkdown('测试世界', 'world_book', fields);

    expect(md).not.toContain('## 自定义补充设定');
  });

  it('skips empty custom field values', () => {
    const fields: PartnerItemFields = {
      customFields: [
        { id: 'cf-7', moduleId: 'world_basic', label: '有效字段', value: '有值' },
        { id: 'cf-8', moduleId: 'world_basic', label: '空字段', value: '' },
        { id: 'cf-9', moduleId: 'world_basic', label: '空白字段', value: '   ' },
      ],
    };

    const md = compileItemToMarkdown('测试世界', 'world_book', fields);

    expect(md).toContain('- **有效字段**：有值');
    expect(md).not.toContain('空字段');
    expect(md).not.toContain('空白字段');
  });

  it('preserves predefined sections alongside custom fields', () => {
    const fields: PartnerItemFields = {
      theme: '奇幻',
      geography: '大陆中央有巨树',
      customFields: [
        { id: 'cf-10', moduleId: 'world_basic', label: '语言', value: '通用语与古精灵语' },
      ],
    };

    const md = compileItemToMarkdown('测试世界', 'world_book', fields);

    expect(md).toContain('## 核心设定');
    expect(md).toContain('- **主题**：奇幻');
    expect(md).toContain('## 地理格局');
    expect(md).toContain('大陆中央有巨树');
    expect(md).toContain('## 自定义补充设定');
    expect(md).toContain('- **语言**：通用语与古精灵语');
  });
});
