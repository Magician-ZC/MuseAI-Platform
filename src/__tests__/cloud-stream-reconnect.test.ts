// 回归 cloudStream 断线重连补偿参数 bug：服务端 StreamQuery.last_event_id: Option<i64> 收 sequence（i64），
// 旧实现用 payload.id(string) 存 lastEventId 且参数名 lastEventId，导致重连补偿完全失效。
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { cloudStream } from '../utils/cloudApi';

class MockWS {
  static instances: MockWS[] = [];
  url: string;
  onmessage: ((e: { data: string }) => void) | null = null;
  onerror: ((e: unknown) => void) | null = null;
  onclose: (() => void) | null = null;
  readyState = 1;
  constructor(url: string) {
    this.url = url;
    MockWS.instances.push(this);
  }
  close() {
    this.readyState = 3;
  }
}

beforeEach(() => {
  MockWS.instances = [];
  vi.stubGlobal('WebSocket', MockWS as unknown as typeof WebSocket);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('cloudStream 断线重连补偿', () => {
  it('收到 {sequence:5} 后重连，URL 带 last_event_id=5（非 lastEventId、非 id 字符串）', () => {
    vi.useFakeTimers();
    try {
      const events: unknown[] = [];
      const unsub = cloudStream('w1', (e) => events.push(e));

      // 首连：单条连接，未带补偿游标。
      expect(MockWS.instances).toHaveLength(1);
      const first = MockWS.instances[0];
      expect(first.url).toContain('/api/worlds/w1/stream');
      expect(first.url).not.toContain('last_event_id');

      // 收到一条 sequence=5 的事件（同时带 id 字符串，验证用的是 sequence 而非 id）。
      first.onmessage?.({ data: JSON.stringify({ id: 'we_abc', sequence: 5, type: 'action' }) });
      expect(events).toHaveLength(1);

      // 断线 → 2s 后自动重连。
      first.onclose?.();
      vi.advanceTimersByTime(2000);

      expect(MockWS.instances).toHaveLength(2);
      const second = MockWS.instances[1];
      // 关键断言：补偿参数名对齐 last_event_id，值为 sequence(5)，且不再是旧的 lastEventId/字符串 id。
      expect(second.url).toContain('last_event_id=5');
      expect(second.url).not.toContain('lastEventId=');
      expect(second.url).not.toContain('we_abc');

      unsub();
    } finally {
      vi.useRealTimers();
    }
  });
});
