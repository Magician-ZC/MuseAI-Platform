import { render, screen, fireEvent } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import Background from '../pages/Background';
import { usePartnerStore } from '../stores/usePartnerStore';
import { useKnowledgePackStore } from '../stores/useKnowledgePackStore';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

const mockInvoke = invoke as unknown as Mock;

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (cmd: string) => {
    if (cmd === 'list_knowledge_packs') return [];
    if (cmd === 'list_knowledge_bindings') return [];
    return undefined;
  });
  usePartnerStore.setState({
    worldBooks: [],
    characterCards: [],
    characterCardsV2: [],
    selectedId: null,
    selectedType: null,
  });
  useKnowledgePackStore.setState({ packs: [], bindings: [], fragments: [] });
});

describe('Background P0.b / P1 entries', () => {
  it('renders both entry buttons and opens the extraction wizard', () => {
    render(<Background />);
    expect(screen.getByRole('button', { name: /全书提取角色/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /知识包/ })).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: /全书提取角色/ }));
    expect(screen.getByText('全书角色提取向导')).toBeInTheDocument();
  });

  it('opens the knowledge pack manager from the header button', () => {
    render(<Background />);
    fireEvent.click(screen.getByRole('button', { name: /知识包/ }));
    expect(screen.getByText('知识包管理')).toBeInTheDocument();
  });
});
