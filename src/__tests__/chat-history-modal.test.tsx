import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { Modal } from 'antd';
import Chat from '../pages/Chat';
import { usePartnerChatStore } from '../stores/usePartnerChatStore';
import { usePartnerStore } from '../stores/usePartnerStore';
import { useSettingsStore } from '../stores/useSettingsStore';
import { useStylePresetStore } from '../stores/useStylePresetStore';

const worldBook = {
  id: 'wb-chat-1',
  name: '雾城世界',
  type: 'world_book' as const,
  content: '# 雾城世界',
  fields: {},
};

const secondWorldBook = {
  id: 'wb-chat-2',
  name: '海港世界',
  type: 'world_book' as const,
  content: '# 海港世界',
  fields: {},
};

const characterCard = {
  id: 'cc-chat-1',
  name: '洛桑',
  type: 'character_card' as const,
  content: '# 洛桑',
  fields: {},
  worldBookId: worldBook.id,
};

const fallbackCharacterCard = {
  id: 'cc-chat-2',
  name: '琥珀',
  type: 'character_card' as const,
  content: '# 琥珀',
  fields: {},
  worldBookId: worldBook.id,
};

const harborCharacterCard = {
  id: 'cc-chat-3',
  name: '渡鸦',
  type: 'character_card' as const,
  content: '# 渡鸦',
  fields: {},
  worldBookId: secondWorldBook.id,
};

let sessionSummaries: any[] = [];

const defaultInvoke = async (command: string, args?: any) => {
  if (command === 'list_agent_sessions') return sessionSummaries;
  if (command === 'load_agent_session') {
    return {
      id: args.id,
      title: '雾城夜谈',
      savedAt: 1717951140000,
      messages: [{ id: 'm1', role: 'user', content: '继续说', tools: [] }],
      selectedReferenceFiles: [],
      selectedOutlineFile: null,
      todos: [],
      isArchived: true,
      characterCardId: characterCard.id,
      selectedWorldBookId: worldBook.id,
    };
  }
  if (command === 'delete_agent_session') return null;
  if (command === 'update_agent_session_title') {
    const session = sessionSummaries.find((item) => item.id === args.id);
    if (session) session.title = args.title;
    return { ...session, savedAt: Date.now() };
  }
  if (command === 'summarize_text') return '新保存标题';
  if (command === 'start_chat_completion_stream') return 'run-1';
  if (command === 'save_agent_session') return { id: args.session.id, title: args.session.title, savedAt: Date.now() };
  return undefined;
};

const invokeMock = vi.fn(defaultInvoke);

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string, args?: any) => invokeMock(command, args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: async () => () => {},
}));

function resetStores() {
  useStylePresetStore.setState({ presets: [] });
  sessionSummaries = [
    {
      id: 'partner-session-1',
      title: '雾城夜谈',
      savedAt: 1717951140000,
      characterCardId: characterCard.id,
      selectedWorldBookId: worldBook.id,
    },
    {
      id: 'partner-session-2',
      title: '港口来信',
      savedAt: 1717864800000,
      characterCardId: harborCharacterCard.id,
      selectedWorldBookId: secondWorldBook.id,
    },
    {
      id: 'partner-session-3',
      title: '琥珀旧梦',
      savedAt: 1717778400000,
      characterCardId: fallbackCharacterCard.id,
      selectedWorldBookId: null,
    },
  ];
  usePartnerStore.setState({
    worldBooks: [worldBook, secondWorldBook],
    characterCards: [characterCard, fallbackCharacterCard, harborCharacterCard],
    selectedId: null,
    selectedType: null,
  });
  usePartnerChatStore.setState({
    messages: [],
    input: '',
    isStreaming: false,
    expandedBlocks: {},
    selectedWorldBookId: worldBook.id,
    selectedCharacterCardId: characterCard.id,
    userInfo: {},
    sessions: [],
    sessionId: 'partner-session-current',
    sessionTitle: '新聊天',
    activeRun: { runId: null, messageId: null },
    isSessionArchived: false,
    contextCompaction: null,
    selectedStylePresetIds: [],
    initialStylePresetIds: [],
    initialSystemPromptSnapshot: null,
  });
  useSettingsStore.setState({
    agentConfigs: {
      ...useSettingsStore.getState().agentConfigs,
      partnerChat: {
        ...useSettingsStore.getState().agentConfigs.partnerChat,
        frequencyPenalty: 0.55,
        presencePenalty: 0.45,
        topP: 0.75,
      },
    },
  });
  invokeMock.mockReset();
  invokeMock.mockImplementation(defaultInvoke);
}

describe('Chat history modal', () => {
  beforeEach(() => {
    Modal.destroyAll();
    document.body.innerHTML = '';
    resetStores();
  });

  it('开始聊天后隐藏文风预设和伴侣设置入口', () => {
    usePartnerChatStore.setState({
      messages: [{ id: 'started-message', role: 'user', content: '你好', tools: [] }],
    });

    render(<Chat />);

    expect(screen.queryByLabelText('聊天文风预设')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '伴侣设置' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: '查看上下文详情' }).closest('.agent-composer__actions')).toHaveStyle({
      justifyContent: 'flex-end',
    });
  });

  it('renders session metadata, filters by World Book and Character Card, loads, and deletes from the modal', async () => {
    render(<Chat />);

    fireEvent.click(screen.getByRole('button', { name: '历史记录' }));

    expect(await screen.findByText('历史聊天')).toBeInTheDocument();
    let dialog = screen.getByRole('dialog');
    expect(within(dialog).getByText('雾城夜谈')).toBeInTheDocument();
    expect(within(dialog).getAllByText(/雾城世界/)).not.toHaveLength(0);
    expect(within(dialog).getByText(/洛桑/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: '打开雾城夜谈' }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('load_agent_session', { id: 'partner-session-1' });
    });

    fireEvent.click(screen.getByRole('button', { name: '历史记录' }));
    expect(await screen.findByText('历史聊天')).toBeInTheDocument();
    dialog = screen.getByRole('dialog');

    fireEvent.mouseDown(screen.getByLabelText('按世界书筛选'));
    const worldBookOptions = await screen.findAllByText('雾城世界');
    fireEvent.click(worldBookOptions[worldBookOptions.length - 1]);
    expect(within(dialog).queryByText('港口来信')).not.toBeInTheDocument();
    expect(within(dialog).getByText('琥珀旧梦')).toBeInTheDocument();

    fireEvent.mouseDown(screen.getByLabelText('按角色卡筛选'));
    const characterOptions = await screen.findAllByText('琥珀');
    fireEvent.click(characterOptions[characterOptions.length - 1]);
    expect(within(dialog).queryByText('雾城夜谈')).not.toBeInTheDocument();
    expect(within(dialog).getByText('琥珀旧梦')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: '删除琥珀旧梦' }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('delete_agent_session', { id: 'partner-session-3' });
    });
  });

  it('saves selectedWorldBookId on partner sessions', async () => {
    usePartnerChatStore.setState({
      messages: [{ id: 'm1', role: 'user', content: '记住这个世界', tools: [] }],
      selectedWorldBookId: worldBook.id,
      selectedCharacterCardId: characterCard.id,
    });
    render(<Chat />);

    fireEvent.click(screen.getByRole('button', { name: /保存对话/ }));
    expect(await screen.findByRole('dialog')).toHaveTextContent('保存对话');
    fireEvent.click(screen.getByRole('button', { name: '确认保存' }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        'save_agent_session',
        expect.objectContaining({
          session: expect.objectContaining({
            selectedWorldBookId: worldBook.id,
            characterCardId: characterCard.id,
          }),
        }),
      );
    });
  });

  it('opens a save choice modal and saves chat as a new editable record', async () => {
    usePartnerChatStore.setState({
      sessionId: 'partner-session-draft',
      sessionTitle: '新聊天',
      messages: [{ id: 'm1', role: 'user', content: '请记住今晚的雾城', tools: [] }],
      selectedWorldBookId: worldBook.id,
      selectedCharacterCardId: characterCard.id,
    });
    render(<Chat />);

    fireEvent.click(screen.getByRole('button', { name: /保存对话/ }));
    expect(await screen.findByRole('dialog')).toHaveTextContent('保存对话');
    expect(invokeMock).not.toHaveBeenCalledWith('save_agent_session', expect.anything());
    expect(screen.getByLabelText('覆盖原记录')).not.toBeDisabled();
    expect(screen.getByLabelText('覆盖记录')).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText('保存为新记录'));

    const titleInput = screen.getByRole('textbox', { name: '会话名称' });
    fireEvent.change(titleInput, { target: { value: '雾城分支存档' } });
    fireEvent.click(screen.getByRole('button', { name: '确认保存' }));

    await waitFor(() => {
      const saveCall = invokeMock.mock.calls.find(([command]) => command === 'save_agent_session');
      const saveArgs = saveCall?.[1] as any;
      expect(saveArgs.session.id).toMatch(/^partner-session-/);
      expect(saveArgs.session.id).not.toBe('partner-session-draft');
      expect(saveArgs.session.title).toBe('雾城分支存档');
      expect(usePartnerChatStore.getState().sessionId).toBe(saveArgs.session.id);
      expect(usePartnerChatStore.getState().sessionTitle).toBe('雾城分支存档');
    });
  });

  it('allows a draft chat to overwrite an existing history record', async () => {
    usePartnerChatStore.setState({
      sessionId: 'partner-session-draft',
      sessionTitle: '新聊天',
      messages: [{ id: 'm1', role: 'user', content: '覆盖旧的雾城记录', tools: [] }],
      selectedWorldBookId: worldBook.id,
      selectedCharacterCardId: characterCard.id,
    });
    render(<Chat />);
    await waitFor(() => {
      expect(usePartnerChatStore.getState().sessions.length).toBeGreaterThan(0);
    });

    fireEvent.click(screen.getByRole('button', { name: /保存对话/ }));
    expect(await screen.findByRole('dialog')).toHaveTextContent('保存对话');
    expect(screen.getByLabelText('覆盖原记录')).not.toBeDisabled();
    expect(screen.getByText('雾城夜谈')).toBeInTheDocument();
    fireEvent.mouseDown(screen.getByLabelText('覆盖记录'));
    expect(await screen.findAllByText(/保存于/)).not.toHaveLength(0);
    expect(screen.getAllByText('世界书：雾城世界').length).toBeGreaterThan(0);
    expect(screen.getAllByText('角色卡：洛桑').length).toBeGreaterThan(0);
    fireEvent.click(screen.getByRole('button', { name: '确认保存' }));

    await waitFor(() => {
      const saveCall = invokeMock.mock.calls.find(([command]) => command === 'save_agent_session');
      const saveArgs = saveCall?.[1] as any;
      expect(saveArgs.session.id).toBe('partner-session-1');
      expect(usePartnerChatStore.getState().sessionId).toBe('partner-session-1');
    });
  });

  it('overwrites the existing chat record from the save choice modal', async () => {
    usePartnerChatStore.setState({
      sessionId: 'partner-session-1',
      sessionTitle: '雾城夜谈',
      messages: [{ id: 'm1', role: 'user', content: '继续说', tools: [] }],
      selectedWorldBookId: worldBook.id,
      selectedCharacterCardId: characterCard.id,
    });
    render(<Chat />);
    await waitFor(() => {
      expect(usePartnerChatStore.getState().sessions.some((item) => item.id === 'partner-session-1')).toBe(true);
    });

    fireEvent.click(screen.getByRole('button', { name: /保存对话/ }));
    expect(await screen.findByRole('dialog')).toHaveTextContent('保存对话');
    fireEvent.click(screen.getByLabelText('覆盖原记录'));
    fireEvent.click(screen.getByRole('button', { name: '确认保存' }));

    await waitFor(() => {
      const saveCall = invokeMock.mock.calls.find(([command]) => command === 'save_agent_session');
      expect((saveCall?.[1] as any).session.id).toBe('partner-session-1');
    });
  });

  it('renames a history session without opening it', async () => {
    usePartnerChatStore.setState({
      sessionId: 'partner-session-1',
      sessionTitle: '雾城夜谈',
    });
    render(<Chat />);

    fireEvent.click(screen.getByRole('button', { name: '历史记录' }));
    expect(await screen.findByText('历史聊天')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: '修改雾城夜谈的名称' }));
    const input = screen.getByRole('textbox', { name: '会话名称' });
    fireEvent.change(input, { target: { value: '  雾城重逢  ' } });
    fireEvent.click(screen.getByRole('button', { name: '确认修改名称' }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('update_agent_session_title', {
        id: 'partner-session-1',
        title: '雾城重逢',
      });
    });
    expect(invokeMock).not.toHaveBeenCalledWith('load_agent_session', { id: 'partner-session-1' });
    expect(await screen.findByText('雾城重逢')).toBeInTheDocument();
    expect(usePartnerChatStore.getState().sessionTitle).toBe('雾城重逢');
  });

  it('keeps an existing session title without summarizing it again', async () => {
    usePartnerChatStore.setState({
      sessionTitle: '用户自定义标题',
      messages: [{ id: 'm1', role: 'user', content: '继续聊天', tools: [] }],
    });
    render(<Chat />);

    fireEvent.click(screen.getByRole('button', { name: /保存对话/ }));
    expect(await screen.findByRole('dialog')).toHaveTextContent('保存对话');
    fireEvent.click(screen.getByRole('button', { name: '确认保存' }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        'save_agent_session',
        expect.objectContaining({
          session: expect.objectContaining({ title: '用户自定义标题' }),
        }),
      );
    });
    expect(invokeMock).not.toHaveBeenCalledWith('summarize_text', expect.anything());
  });

  it('falls back to the first user message when title generation fails', async () => {
    invokeMock.mockImplementation(async (command: string, args?: any) => {
      if (command === 'save_app_state') return undefined;
      if (command === 'list_agent_sessions') return sessionSummaries;
      if (command === 'summarize_text') throw new Error('生成标题为空');
      if (command === 'save_agent_session') return { id: args.session.id, title: args.session.title, savedAt: Date.now() };
      return undefined;
    });
    usePartnerChatStore.setState({
      sessionTitle: '新聊天',
      messages: [{ id: 'm1', role: 'user', content: '   请陪我聊聊今天发生的事情   ', tools: [] }],
    });
    render(<Chat />);

    fireEvent.click(screen.getByRole('button', { name: /保存对话/ }));
    expect(await screen.findByRole('dialog')).toHaveTextContent('保存对话');
    fireEvent.click(screen.getByRole('button', { name: '确认保存' }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        'save_agent_session',
        expect.objectContaining({
          session: expect.objectContaining({ title: '请陪我聊聊今天发生的事情' }),
        }),
      );
    });
  });

  it('repairs an empty partner session id before saving', async () => {
    usePartnerChatStore.setState({
      sessionId: '',
      messages: [{ id: 'm1', role: 'user', content: '记住这个世界', tools: [] }],
      selectedWorldBookId: worldBook.id,
      selectedCharacterCardId: characterCard.id,
    });
    render(<Chat />);

    fireEvent.click(screen.getByRole('button', { name: /保存对话/ }));
    expect(await screen.findByRole('dialog')).toHaveTextContent('保存对话');
    fireEvent.click(screen.getByRole('button', { name: '确认保存' }));

    await waitFor(() => {
      const saveCall = invokeMock.mock.calls.find(([command]) => command === 'save_agent_session');
      expect(saveCall?.[1].session.id).toMatch(/^partner-session-/);
      expect(usePartnerChatStore.getState().sessionId).toBe(saveCall?.[1].session.id);
    });
  });

  it('passes partnerChat agent id when sending desktop chat messages', async () => {
    useStylePresetStore.setState({ presets: [
      { id: 'chat-1', name: '克制对白', segments: [{ id: 'chat-segment-1', title: '对白', content: '聊天预设正文', enabled: true }], createdAt: 1, updatedAt: 1 },
    ] });
    usePartnerChatStore.setState({
      input: '今晚聊聊雾城',
      selectedWorldBookId: worldBook.id,
      selectedCharacterCardId: characterCard.id,
      selectedStylePresetIds: ['chat-1'],
    });
    const { container } = render(<Chat />);

    const sendButton = container.querySelector('.de-ai-agent-run-button') as HTMLButtonElement | null;
    expect(sendButton).not.toBeNull();
    fireEvent.click(sendButton!);

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        'start_chat_completion_stream',
        expect.objectContaining({
          request: expect.objectContaining({
            agentId: 'partnerChat',
            frequencyPenalty: 0.55,
            presencePenalty: 0.45,
            topP: 0.75,
            systemPrompt: expect.stringMatching(/聊天预设正文[\s\S]*伴侣/),
          }),
        }),
      );
    });
    expect(usePartnerChatStore.getState().initialSystemPromptSnapshot).toContain('聊天预设正文');
  });
});
