import '@testing-library/jest-dom/vitest';
import { act, cleanup } from '@testing-library/react';
import { message, notification } from 'antd';
import { afterAll, afterEach, vi } from 'vitest';

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

/// 把 React 的待办工作彻底排干。
///
/// 背景：React 19 的 scheduler 在 Node 下用 `setImmediate` 排渲染工作。若有残留工作拖到
/// vitest 销毁 jsdom 之后才执行，就会在 react-dom 内部撞上 `ReferenceError: window is not defined`。
/// 该错误**不计入失败用例**，但会让 vitest 以退出码 1 结束——表现为「479 passed 却整体失败」，
/// 且因为是竞态而**偶发**（本地时绿时红，CI 慢机器上更容易触发），属于最难查的一类 flaky。
///
/// 两段式：先 `act` 逼出 React 内部的 pending 渲染与 passive effect（单纯等宏任务不保证它们已入队），
/// 再连续 flush 若干轮宏任务，把 act 触发出来的新一轮工作、以及 antd 动画 / ECharts 续排的工作排空。
const drainReactWork = async (rounds: number) => {
  // 组件已卸载时 act 内部无渲染可 flush，属正常路径；异常不应影响清理。
  // 注意必须用 try/catch 而不是 `.catch()`——act 会检测自身是否被 await，
  // 挂 `.catch()` 会让它误判为「未 await」并打印警告、干扰 act 作用域。
  try {
    await act(async () => {});
  } catch {
    /* 无待处理渲染，忽略 */
  }
  for (let i = 0; i < rounds; i += 1) {
    await flushMacrotask();
  }
};

afterEach(async () => {
  cleanup();
  // antd 的 message / notification 是**独立于组件树的全局单例**：各自用自己的 createRoot 挂在
  // document 上，自带自动关闭定时器与出入场动画，`cleanup()` 卸载被测组件时清不掉它们。
  // 它们残留的定时器会在环境销毁后继续唤醒 React 调度，是本文件那个 teardown 竞态的主要来源
  // （报错反复归属于用了这些全局 API 的测试文件）。
  message.destroy();
  notification.destroy();
  await drainReactWork(5);
});

/// afterEach 覆盖不到「最后一个用例结束 → vitest 销毁环境」这段窗口——
/// 上面那个 ReferenceError 正是在这里逃逸的（报错归属的测试文件每次都不同，
/// 说明它是全局竞态而非某个用例的问题）。故在文件级再排一次，轮数给足。
afterAll(async () => {
  await drainReactWork(10);
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
