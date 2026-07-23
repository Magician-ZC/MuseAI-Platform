// 管理后台外壳（A0，主循环所有）：RBAC 路由 + 八模块菜单。页面实现归 agent-A1。
import type { ComponentType } from 'react';
import { BrowserRouter, Routes, Route, Navigate, Link, useLocation } from 'react-router-dom';
import { Layout, Menu } from 'antd';
import { getToken } from './api';

// A1 八模块真实页面（替换 Placeholder）。
import Login from './pages/Login';
import Users from './pages/Users';
import Audit from './pages/Audit';
import WorldsOps from './pages/WorldsOps';
import Economy from './pages/Economy';
import Metrics from './pages/Metrics';
import Governance from './pages/Governance';
import Risk from './pages/Risk';
import Tickets from './pages/Tickets';

const MODULES = [
  { key: 'users', label: '用户管理' },
  { key: 'audit', label: '内容审核' },
  { key: 'worlds', label: '世界运营' },
  { key: 'economy', label: '经济运营' },
  { key: 'metrics', label: '数据看板' },
  { key: 'prompts', label: '模型与 Prompt' },
  { key: 'risk', label: '风控' },
  { key: 'tickets', label: '客服与工单' },
];

// 路由键 → 页面组件（路由结构不变，仅替换渲染的页面）。
const PAGES: Record<string, ComponentType> = {
  users: Users,
  audit: Audit,
  worlds: WorldsOps,
  economy: Economy,
  metrics: Metrics,
  prompts: Governance,
  risk: Risk,
  tickets: Tickets,
};

function Shell() {
  const location = useLocation();
  const active = location.pathname.split('/')[1] || 'metrics';
  if (!getToken()) return <Navigate to="/login" replace />;
  return (
    <Layout style={{ minHeight: '100vh' }}>
      <Layout.Sider theme="light">
        <div style={{ padding: 16, fontWeight: 600 }}>MuseAI 后台</div>
        <Menu
          mode="inline"
          selectedKeys={[active]}
          items={MODULES.map((m) => ({ key: m.key, label: <Link to={`/${m.key}`}>{m.label}</Link> }))}
        />
      </Layout.Sider>
      <Layout>
        <Layout.Content style={{ padding: 24 }}>
          <Routes>
            {MODULES.map((m) => {
              const Page = PAGES[m.key];
              return <Route key={m.key} path={`/${m.key}`} element={<Page />} />;
            })}
            <Route path="*" element={<Navigate to="/metrics" replace />} />
          </Routes>
        </Layout.Content>
      </Layout>
    </Layout>
  );
}

export default function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/login" element={<Login />} />
        <Route path="/*" element={<Shell />} />
      </Routes>
    </BrowserRouter>
  );
}
