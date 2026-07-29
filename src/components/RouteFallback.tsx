// 路由级懒加载的等待态（src/App.tsx 的 43 条路由全部按页分包，见那里的文件头）。
//
// 🔴 它只包 `<Outlet />`，不包整个壳：壳（侧栏 / 顶栏 / 底部导航）必须在切页时保持不动，
//    否则每次点导航整屏都会闪一下——那比多等一个 chunk 更像“卡了”。
//
// 视觉沿用仓库里**已经在跑**的那一个懒加载 fallback（`MarkdownEditor.tsx`），
// 不新造设计：本次改的是加载策略，不该顺手引入一个新的等待态样式。
import { Suspense, type ReactNode } from 'react';
import { Spin } from 'antd';

export default function RouteFallback({ children }: { children: ReactNode }) {
  return (
    <Suspense
      fallback={
        <div style={{ height: '100%', display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 48 }}>
          <Spin />
        </div>
      }
    >
      {children}
    </Suspense>
  );
}
