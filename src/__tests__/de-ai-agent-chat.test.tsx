import { useState } from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import DeAiAgentChat from '../components/DeAiAgentChat';
import type { Message } from '../stores/useAgentStore';

type StreamPayload = {
  runId: string;
  eventType: 'done';
};

let streamListener: ((event: { payload: StreamPayload }) => void) | null = null;
let runCounter = 0;
let listenCount = 0;

const invokeMock = vi.fn(async (command: string, _args?: unknown) => {
  if (command === 'build_full_system_prompt') return '完整系统提示词';
  if (command === 'start_chat_completion_stream') {
    runCounter += 1;
    return `run-${runCounter}`;
  }
  return undefined;
});

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string, args?: unknown) => invokeMock(command, args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: async (_event: string, handler: (event: { payload: StreamPayload }) => void) => {
    listenCount += 1;
    streamListener = handler;
    return () => {
      streamListener = null;
    };
  },
}));

function Harness({ onDone }: { onDone: (lastAgentMessage: string) => string | void }) {
  const [messages, setMessages] = useState<Message[]>([]);
  const [activeRun, setActiveRun] = useState<{ runId: string | null; messageId: string | null }>({
    runId: null,
    messageId: null,
  });
  const [isRunning, setIsRunning] = useState(false);

  return (
    <DeAiAgentChat
      title="AI 味检测 Agent"
      agentId="detector"
      systemPrompt="系统提示词"
      allowedTools={['read']}
      startContent="开始检测"
      messages={messages}
      setMessages={setMessages}
      activeRun={activeRun}
      setActiveRun={setActiveRun}
      isRunning={isRunning}
      onRunningChange={setIsRunning}
      onDone={onDone}
    />
  );
}

describe('DeAiAgentChat', () => {
  beforeEach(() => {
    streamListener = null;
    runCounter = 0;
    listenCount = 0;
    invokeMock.mockClear();
  });

  it('uses the latest committed completion callback without duplicating the stream listener', async () => {
    const initialOnDone = vi.fn(() => undefined);
    const latestOnDone = vi.fn(() => '请按最新检测规则重试');
    const { rerender } = render(<Harness onDone={initialOnDone} />);

    fireEvent.click(screen.getByRole('button', { name: '开始' }));

    await waitFor(() => {
      expect(streamListener).not.toBeNull();
      expect(runCounter).toBe(1);
    });

    rerender(<Harness onDone={latestOnDone} />);

    await act(async () => {
      streamListener?.({ payload: { runId: 'run-1', eventType: 'done' } });
    });

    await waitFor(() => {
      expect(initialOnDone).not.toHaveBeenCalled();
      expect(latestOnDone).toHaveBeenCalledTimes(1);
      expect(runCounter).toBe(2);
      expect(listenCount).toBe(1);
    });
  });
});
