import { beforeEach, describe, expect, it } from 'vitest';
import { usePartnerStore } from '../stores/usePartnerStore';
import { applyPartnerStoreContent } from '../utils/partnerStoreSync';

describe('partner store sync', () => {
  beforeEach(() => {
    usePartnerStore.setState({
      worldBooks: [],
      characterCards: [],
      selectedId: null,
      selectedType: null,
    });
  });

  it('applies persisted partner state into the desktop store', () => {
    const applied = applyPartnerStoreContent(JSON.stringify({
      state: {
        worldBooks: [],
        characterCards: [
          {
            id: 'card-1',
            name: '禾禾',
            type: 'character_card',
            content: '# 角色卡：禾禾',
            fields: {
              userRelationType: '伙伴',
              keyEvents: '共同完成一次对话',
            },
          },
        ],
        selectedId: null,
        selectedType: null,
      },
      version: 0,
    }));

    expect(applied).toBe(true);
    expect(usePartnerStore.getState().characterCards[0].fields?.userRelationType).toBe('伙伴');
    expect(usePartnerStore.getState().characterCards[0].fields?.keyEvents).toBe('共同完成一次对话');
  });

  it('normalizes generated non-string fields before saving character cards', () => {
    usePartnerStore.getState().importGeneratedItems({
      worldBooks: [],
      characterCards: [
        {
          name: '陆雪莹',
          fields: {
            keyEvents: ['第一次见面', '共同调查以太风暴'],
            speakingStyle: { tone: '冷静', catchphrase: '别怕，我在' },
          } as any,
        },
      ],
    });

    const card = usePartnerStore.getState().characterCards[0];
    expect(card.fields?.keyEvents).toBe('第一次见面\n共同调查以太风暴');
    expect(card.fields?.speakingStyle).toContain('"tone"');
    expect(card.content).toContain('第一次见面');
  });
});
