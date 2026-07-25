import '@testing-library/jest-dom/vitest';
import { cleanup } from '@testing-library/react';
import { afterEach, vi } from 'vitest';

/// 排空一轮宏任务队列。React 19 的 scheduler 在 Node 下用 setImmediate 排渲染工作。
const flushMacrotask = () =>
  new Promise<void>((resolve) => {
    const scheduleImmediate = Reflect.get(globalThis, 'setImmediate') as
      | ((callback: () => void) => void)
      | undefined;
    if (scheduleImmediate) {
      scheduleImmediate(resolve);
    } else {
      setTimeout(resolve, 0);
    }
  });

afterEach(async () => {
  cleanup();
  // 只 flush 一轮不够：cleanup() 的 unmount 会让 React 再排一轮 performWorkUntilDeadline，
  // 第三方组件（antd 动画 / ECharts）还可能续排。残留工作若拖到 vitest 销毁 jsdom 之后执行，
  // 就会在 react-dom 内部撞上 `ReferenceError: window is not defined`——
  // 该错误不计入失败用例，但会让 vitest 以退出码 1 结束（本地偶发、CI 慢机器上必现）。
  // 连续 flush 数轮把队列排干，比只等一轮稳得多。
  for (let i = 0; i < 5; i += 1) {
    await flushMacrotask();
  }
});

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(() => Promise.resolve()),
}));

const storage = (() => {
  let values: Record<string, string> = {};
  return {
    getItem: (key: string) => values[key] ?? null,
    setItem: (key: string, value: string) => {
      values[key] = value;
    },
    removeItem: (key: string) => {
      delete values[key];
    },
    clear: () => {
      values = {};
    },
  };
})();

Object.defineProperty(globalThis, 'localStorage', {
  value: storage,
  configurable: true,
});

class ResizeObserverMock {
  observe() {}
  unobserve() {}
  disconnect() {}
}

Object.defineProperty(globalThis, 'ResizeObserver', {
  value: ResizeObserverMock,
  configurable: true,
});

Object.defineProperty(window, 'matchMedia', {
  value: vi.fn().mockImplementation((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: vi.fn(),
    removeListener: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })),
  configurable: true,
});
