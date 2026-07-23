import { describe, expect, it, beforeEach } from 'vitest';
import { act } from '@testing-library/react';
import { useStoryStore } from '../stores/useStoryStore';
import {
  buildStoryModelMessages,
  compileStorySystemPrompt,
  getRolePlayCharacterName,
} from '../pages/storyAgent';
import type { Message } from '../stores/useAgentStore';

describe('Story dynamic role loading store state', () => {
  beforeEach(() => {
    useStoryStore.setState({
      messages: [],
      input: '',
      inputMode: 'speech',
      isStreaming: false,
      expandedBlocks: {},
      selectedWorldBookId: null,
      selectedCharacterCardIds: [],
      sessionTitle: '新故事',
      isSessionArchived: false,
      initialPlot: '',
      contextCompaction: null,
      dynamicRoleLoadingEnabled: false,
    });
  });

  it('defaults dynamic role loading to disabled and resets it for new sessions', () => {
    expect(useStoryStore.getState().dynamicRoleLoadingEnabled).toBe(false);

    act(() => {
      useStoryStore.getState().setDynamicRoleLoadingEnabled(true);
    });
    expect(useStoryStore.getState().dynamicRoleLoadingEnabled).toBe(true);

    act(() => {
      useStoryStore.getState().createNewSession();
    });
    expect(useStoryStore.getState().dynamicRoleLoadingEnabled).toBe(false);
  });
});

describe('Story prompt compilation', () => {
  it('keeps existing multi-card injection when dynamic loading is disabled', () => {
    const prompt = compileStorySystemPrompt({
      basePrompt: '故事主持人',
      worldBookContent: '# 世界书\n魔法大陆',
      characterCards: [
        { name: '林逸', content: '林逸角色卡正文' },
        { name: '陆雪莹', content: '陆雪莹角色卡正文' },
      ],
      userInfo: { name: '阿明', skills: '风系魔法' },
      dynamicRoleLoadingEnabled: false,
    });

    expect(prompt).toContain('故事主持人');
    expect(prompt).toContain('魔法大陆');
    expect(prompt).toContain('林逸角色卡正文');
    expect(prompt).toContain('陆雪莹角色卡正文');
    expect(prompt).toContain('阿明');
    expect(prompt).toContain('风系魔法');
  });

  it('keeps role card bodies and user info when dynamic loading is enabled', () => {
    const prompt = compileStorySystemPrompt({
      basePrompt: '故事主持人',
      worldBookContent: '# 世界书\n魔法大陆',
      characterCards: [
        { name: '林逸', content: '林逸角色卡正文' },
        { name: '陆雪莹', content: '陆雪莹角色卡正文' },
      ],
      userInfo: { name: '阿明', identityTags: ['穿越者', '法师'] },
      dynamicRoleLoadingEnabled: true,
    });

    expect(prompt).toContain('故事主持人');
    expect(prompt).toContain('魔法大陆');
    expect(prompt).toContain('林逸');
    expect(prompt).toContain('陆雪莹');
    expect(prompt).toContain('role_play');
    expect(prompt).toContain('林逸角色卡正文');
    expect(prompt).toContain('陆雪莹角色卡正文');
    expect(prompt).toContain('阿明');
    expect(prompt).toContain('穿越者、法师');
  });
});

describe('Story model history messages', () => {
  it('preserves assistant tool calls and tool results in protocol-shaped history', () => {
    const messages: Message[] = [
      { id: 'u1', role: 'user', content: '我进入森林。', tools: [] },
      {
        id: 'a1',
        role: 'agent',
        content: '树影晃动。\n\n[[TOOL:tool-1]]\n\n她转过身。',
        tools: [
          {
            id: 'tool-1',
            name: 'role_play',
            arguments: '{"characterName":"陆雪莹"}',
            result: '别乱走。',
            status: 'success',
          },
        ],
      },
    ];

    expect(buildStoryModelMessages(messages)).toEqual([
      { id: 'u1', role: 'user', content: '我进入森林。' },
      {
        id: 'assistant-tool-tool-1',
        role: 'assistant',
        content: '树影晃动。\n\n',
        toolCalls: [{ id: 'tool-1', name: 'role_play', arguments: '{"characterName":"陆雪莹"}' }],
        thinkingBlocks: undefined,
      },
      { id: 'tool-result-tool-1', role: 'tool', content: '别乱走。', toolCallId: 'tool-1' },
      { id: 'a1', role: 'assistant', content: '\n\n她转过身。', thinkingBlocks: undefined },
    ]);
  });
});

describe('Story role_play transcript helpers', () => {
  it('extracts the requested character name from tool arguments', () => {
    expect(getRolePlayCharacterName('{"characterName":"陆雪莹"}')).toBe('陆雪莹');
    expect(getRolePlayCharacterName('{"character_name":"林逸"}')).toBe('林逸');
    expect(getRolePlayCharacterName('{broken')).toBe('角色');
  });
});
