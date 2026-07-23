import { beforeEach, describe, expect, it } from 'vitest';
import { PartnerItem, usePartnerStore } from '../stores/usePartnerStore';

const worldBook = (id: string, name: string, fields: PartnerItem['fields'] = {}): PartnerItem => ({
  id,
  name,
  type: 'world_book',
  content: `# ${name}`,
  fields,
});

const characterCard = (
  id: string,
  name: string,
  worldBookId?: string | null,
  fields: PartnerItem['fields'] = {},
): PartnerItem => ({
  id,
  name,
  type: 'character_card',
  content: `# 角色卡：${name}`,
  fields,
  worldBookId,
});

describe('partner import and export', () => {
  beforeEach(() => {
    usePartnerStore.setState({
      worldBooks: [
        worldBook('wb-1', '云州世界书', {
          theme: '仙侠',
          customFields: [{ id: 'cf-1', moduleId: 'world_basic', label: '货币', value: '灵石' }],
        }),
      ],
      characterCards: [
        characterCard('cc-1', '沈霜', 'wb-1', {
          identityTags: ['剑修'],
          customFields: [{ id: 'cf-2', moduleId: 'char_memory', label: '秘密', value: '知道旧案真相' }],
        }),
      ],
      selectedId: null,
      selectedType: null,
    });
  });

  it('exports World Books with schema, version, timestamp, fields, and custom fields', () => {
    const pkg = usePartnerStore.getState().exportPartnerItems('world_book');

    expect(pkg.schema).toBe('museai.partner-items');
    expect(pkg.version).toBe(1);
    expect(Date.parse(pkg.exportedAt)).not.toBeNaN();
    expect(pkg.worldBooks).toEqual([
      expect.objectContaining({
        id: 'wb-1',
        name: '云州世界书',
        fields: expect.objectContaining({
          theme: '仙侠',
          customFields: [{ id: 'cf-1', moduleId: 'world_basic', label: '货币', value: '灵石' }],
        }),
      }),
    ]);
    expect(pkg.characterCards).toEqual([]);
  });

  it('exports Character Cards with schema, version, timestamp, custom fields, and ownership', () => {
    const pkg = usePartnerStore.getState().exportPartnerItems('character_card');

    expect(pkg.schema).toBe('museai.partner-items');
    expect(pkg.version).toBe(1);
    expect(Date.parse(pkg.exportedAt)).not.toBeNaN();
    expect(pkg.worldBooks).toEqual([]);
    expect(pkg.characterCards).toEqual([
      expect.objectContaining({
        id: 'cc-1',
        name: '沈霜',
        worldBookId: 'wb-1',
        fields: expect.objectContaining({
          identityTags: ['剑修'],
          customFields: [{ id: 'cf-2', moduleId: 'char_memory', label: '秘密', value: '知道旧案真相' }],
        }),
      }),
    ]);
  });

  it('exports a single item without exporting the whole section', () => {
    usePartnerStore.setState({
      worldBooks: [
        worldBook('wb-1', '云州世界书'),
        worldBook('wb-2', '北境世界书'),
      ],
      characterCards: [
        characterCard('cc-1', '沈霜', 'wb-1'),
        characterCard('cc-2', '顾临', 'wb-2'),
      ],
    });

    const worldBookPackage = usePartnerStore.getState().exportPartnerItem('world_book', 'wb-2');
    const characterCardPackage = usePartnerStore.getState().exportPartnerItem('character_card', 'cc-1');

    expect(worldBookPackage.worldBooks.map((item) => item.name)).toEqual(['北境世界书']);
    expect(worldBookPackage.characterCards).toEqual([]);
    expect(characterCardPackage.worldBooks).toEqual([]);
    expect(characterCardPackage.characterCards.map((item) => item.name)).toEqual(['沈霜']);
  });

  it('exports a World Book bundle with its owned Character Cards', () => {
    usePartnerStore.setState({
      worldBooks: [
        worldBook('wb-1', '云州世界书'),
        worldBook('wb-2', '北境世界书'),
      ],
      characterCards: [
        characterCard('cc-1', '沈霜', 'wb-1'),
        characterCard('cc-2', '顾临', 'wb-1'),
        characterCard('cc-3', '无归属角色', null),
        characterCard('cc-4', '北境角色', 'wb-2'),
      ],
    });

    const pkg = usePartnerStore.getState().exportPartnerItemBundle('world_book', 'wb-1');

    expect(pkg.worldBooks.map((item) => item.name)).toEqual(['云州世界书']);
    expect(pkg.characterCards.map((item) => item.name)).toEqual(['沈霜', '顾临']);
  });

  it('imports World Books with normalized fields, compiled markdown, generated ids, and selection', () => {
    usePartnerStore.setState({ worldBooks: [], characterCards: [], selectedId: null, selectedType: null });

    const result = usePartnerStore.getState().importPartnerItemsPackage(JSON.stringify({
      schema: 'museai.partner-items',
      version: 1,
      exportedAt: '2026-06-11T00:00:00.000Z',
      worldBooks: [
        {
          id: 'old-wb',
          name: '镜海世界书',
          fields: {
            theme: ['海城', '悬疑'],
            customFields: [{ id: '', moduleId: 'world_core', label: '', value: 42 }],
          },
        },
      ],
      characterCards: [],
    }), 'world_book');

    const imported = usePartnerStore.getState().worldBooks[0];
    expect(result.worldBookIds).toHaveLength(1);
    expect(imported.id).toMatch(/^wb-import-/);
    expect(imported.id).not.toBe('old-wb');
    expect(imported.fields?.theme).toBe('海城\n悬疑');
    expect(imported.fields?.customFields?.[0]).toEqual(expect.objectContaining({
      moduleId: 'world_core',
      label: '自定义字段',
      value: '42',
    }));
    expect(imported.content).toContain('# 镜海世界书');
    expect(imported.content).toContain('海城');
    expect(usePartnerStore.getState().selectedId).toBe(imported.id);
    expect(usePartnerStore.getState().selectedType).toBe('world_book');
  });

  it('imports a World Book bundle with owned Character Cards remapped to the new World Book id', () => {
    usePartnerStore.setState({ worldBooks: [], characterCards: [], selectedId: null, selectedType: null });

    const result = usePartnerStore.getState().importPartnerItemsPackage(JSON.stringify({
      schema: 'museai.partner-items',
      version: 1,
      exportedAt: '2026-06-11T00:00:00.000Z',
      worldBooks: [{ id: 'old-wb', name: '魔法世界', fields: { theme: '现代魔法' } }],
      characterCards: [
        { id: 'old-card-1', name: '哈利', worldBookId: 'old-wb', fields: { age: '11岁' } },
        { id: 'old-card-2', name: '无关角色', worldBookId: 'other-wb', fields: { age: '20岁' } },
      ],
    }), 'world_book');

    const importedWorldBook = usePartnerStore.getState().worldBooks[0];
    const importedCharacters = usePartnerStore.getState().characterCards;

    expect(result.worldBookIds).toEqual([importedWorldBook.id]);
    expect(result.characterCardIds).toEqual([importedCharacters[0].id]);
    expect(importedWorldBook.id).toMatch(/^wb-import-/);
    expect(importedCharacters).toEqual([
      expect.objectContaining({
        name: '哈利',
        worldBookId: importedWorldBook.id,
      }),
    ]);
    expect(usePartnerStore.getState().selectedId).toBe(importedWorldBook.id);
    expect(usePartnerStore.getState().selectedType).toBe('world_book');
  });

  it('imports Character Cards with valid existing World Book ownership', () => {
    const result = usePartnerStore.getState().importPartnerItemsPackage(JSON.stringify({
      schema: 'museai.partner-items',
      version: 1,
      exportedAt: '2026-06-11T00:00:00.000Z',
      worldBooks: [],
      characterCards: [
        {
          id: 'old-card',
          name: '顾临',
          worldBookId: 'wb-1',
          fields: { identityTags: '谋士，旧友', keyEvents: ['初见', '同行'] },
        },
      ],
    }), 'character_card');

    const cards = usePartnerStore.getState().characterCards;
    const imported = cards[cards.length - 1];
    expect(result.characterCardIds).toHaveLength(1);
    expect(imported).toEqual(expect.objectContaining({
      id: expect.stringMatching(/^cc-import-/),
      name: '顾临',
      worldBookId: 'wb-1',
    }));
    expect(imported?.fields?.identityTags).toEqual(['谋士', '旧友']);
    expect(imported?.fields?.keyEvents).toBe('初见\n同行');
    expect(imported?.content).toContain('# 角色卡：顾临');
    expect(usePartnerStore.getState().selectedId).toBe(imported?.id);
    expect(usePartnerStore.getState().selectedType).toBe('character_card');
  });

  it('imports Character Cards as unassigned when ownership is missing', () => {
    usePartnerStore.getState().importPartnerItemsPackage(JSON.stringify({
      schema: 'museai.partner-items',
      version: 1,
      exportedAt: '2026-06-11T00:00:00.000Z',
      worldBooks: [],
      characterCards: [
        { name: '无归属角色', worldBookId: 'deleted-world', fields: { age: 18 } },
      ],
    }), 'character_card');

    const cards = usePartnerStore.getState().characterCards;
    expect(cards[cards.length - 1]).toEqual(expect.objectContaining({
      name: '无归属角色',
      worldBookId: null,
      fields: expect.objectContaining({ age: '18' }),
    }));
  });

  it('remaps Character Card ownership to same-package imported World Book ids', () => {
    usePartnerStore.setState({ worldBooks: [], characterCards: [], selectedId: null, selectedType: null });

    const result = usePartnerStore.getState().importPartnerItemsPackage(JSON.stringify({
      schema: 'museai.partner-items',
      version: 1,
      exportedAt: '2026-06-11T00:00:00.000Z',
      worldBooks: [{ id: 'old-wb', name: '同包世界', fields: { theme: '奇幻' } }],
      characterCards: [{ id: 'old-card', name: '同包角色', worldBookId: 'old-wb', fields: { age: '19岁' } }],
    }), 'character_card');

    const importedWorldBook = usePartnerStore.getState().worldBooks[0];
    const importedCharacter = usePartnerStore.getState().characterCards[0];
    expect(result.worldBookIds).toEqual([importedWorldBook.id]);
    expect(result.characterCardIds).toEqual([importedCharacter.id]);
    expect(importedWorldBook.id).toMatch(/^wb-import-/);
    expect(importedCharacter.worldBookId).toBe(importedWorldBook.id);
  });

  it('imports multiple package files as one batch so ownership can be remapped', () => {
    usePartnerStore.setState({ worldBooks: [], characterCards: [], selectedId: null, selectedType: null });

    const result = usePartnerStore.getState().importPartnerItemsPackages([
      JSON.stringify({
        schema: 'museai.partner-items',
        version: 1,
        exportedAt: '2026-06-11T00:00:00.000Z',
        worldBooks: [{ id: 'old-wb', name: '批量世界', fields: { theme: '奇幻' } }],
        characterCards: [],
      }),
      JSON.stringify({
        schema: 'museai.partner-items',
        version: 1,
        exportedAt: '2026-06-11T00:00:00.000Z',
        worldBooks: [],
        characterCards: [{ id: 'old-card', name: '批量角色', worldBookId: 'old-wb', fields: { age: '19岁' } }],
      }),
    ], 'character_card');

    const importedWorldBook = usePartnerStore.getState().worldBooks[0];
    const importedCharacter = usePartnerStore.getState().characterCards[0];
    expect(result.failedCount).toBe(0);
    expect(importedWorldBook.name).toBe('批量世界');
    expect(importedCharacter.worldBookId).toBe(importedWorldBook.id);
  });


  it('rejects invalid packages without changing store data', () => {
    const before = usePartnerStore.getState();

    expect(() => {
      usePartnerStore.getState().importPartnerItemsPackage('not json', 'world_book');
    }).toThrow('文件内容不是合法 JSON');

    expect(usePartnerStore.getState().worldBooks).toBe(before.worldBooks);
    expect(usePartnerStore.getState().characterCards).toBe(before.characterCards);
  });
});
