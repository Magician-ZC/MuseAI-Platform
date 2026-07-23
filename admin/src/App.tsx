// 管理后台外壳：#9 RBAC 路由 + 按 role 收敛的八模块菜单。
// 登录后保存 dev-login 返回的 role；菜单与路由按 role 收敛可见模块，越权模块显示「无权限」页。
// 后端 require_role 仍是唯一权威，此处为纵深防御 + UX。
import type { ComponentType } from 'react';
import { BrowserRouter, Routes, Route, Navigate, Link, useLocation, useNavigate } from 'react-router-dom';
import { Button, Layout, Menu, Result, Space, Tag, Typography } from 'antd';
import { clearSession, getRole, getToken } from './api';
import { canAccess, firstModuleKey, MODULES, roleLabel, visibleModules } from './rbac';

// 八模块真实页面。
import Login from './pages/Login';
import Users from './pages/Users';
import Audit from './pages/Audit';
import WorldsOps from './pages/WorldsOps';
import Economy from './pages/Economy';
import Metrics from './pages/Metrics';
import Governance from './pages/Governance';
import Risk from './pages/Risk';
import Tickets from './pages/Tickets';

// 路由键 → 页面组件。
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

/** 越权模块的占位页：不渲染真实页面，给出明确的无权限提示。 */
function Forbidden({ moduleLabel }: { moduleLabel?: string }) {
  return (
    <Result
      status="403"
      title="无权限访问"
      subTitle={`当前登录角色无权访问「${moduleLabel ?? '该模块'}」。如需权限请联系超级管理员。`}
    />
  );
}

/** 角色未分配任何后台模块时的兜底页。 */
function NoModules() {
  const navigate = useNavigate();
  return (
    <Result
      status="403"
      title="无可用模块"
      subTitle="当前角色未分配任何后台模块权限，请联系超级管理员或重新登录。"
      extra={
        <Button
          type="primary"
          onClick={() => {
            clearSession();
            navigate('/login', { replace: true });
          }}
        >
          重新登录
        </Button>
      }
    />
  );
}

function Shell() {
  const location = useLocation();
  const navigate = useNavigate();
  if (!getToken()) return <Navigate to="/login" replace />;

  const role = getRole();
  const visible = visibleModules(role);
  const landing = firstModuleKey(role);
  const active = location.pathname.split('/')[1] || landing || '';

  const logout = () => {
    clearSession();
    navigate('/login', { replace: true });
  };

  return (
    <Layout style={{ minHeight: '100vh' }}>
      <Layout.Sider theme="light">
        <div style={{ padding: 16, fontWeight: 600 }}>MuseAI 后台</div>
        <div style={{ padding: '0 16px 12px' }}>
          <Space size={4} wrap>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              当前角色
            </Typography.Text>
            <Tag color={role === 'admin' ? 'gold' : 'blue'}>{roleLabel(role)}</Tag>
          </Space>
        </div>
        <Menu
          mode="inline"
          selectedKeys={[active]}
          items={visible.map((m) => ({ key: m.key, label: <Link to={`/${m.key}`}>{m.label}</Link> }))}
        />
        <div style={{ padding: 16 }}>
          <Button size="small" block onClick={logout}>
            退出登录
          </Button>
        </div>
      </Layout.Sider>
      <Layout>
        <Layout.Content style={{ padding: 24 }}>
          <Routes>
            {/* 根路径与未知路径落到角色的首个可见模块；无可见模块则兜底页。 */}
            <Route
              index
              element={landing ? <Navigate to={`/${landing}`} replace /> : <NoModules />}
            />
            {MODULES.map((m) => {
              const Page = PAGES[m.key];
              // 越权模块不渲染真实页面（不发越权请求），显示 403 占位页。
              return (
                <Route
                  key={m.key}
                  path={`/${m.key}`}
                  element={canAccess(role, m.key) ? <Page /> : <Forbidden moduleLabel={m.label} />}
                />
              );
            })}
            <Route
              path="*"
              element={landing ? <Navigate to={`/${landing}`} replace /> : <NoModules />}
            />
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
