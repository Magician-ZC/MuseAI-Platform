// 管理后台外壳：角色收敛菜单 + 与客户端一致的 MuseAI 暖色视觉系统。
import { useState, type ComponentType, type ReactNode } from 'react';
import { BrowserRouter, Routes, Route, Navigate, Link, useLocation, useNavigate } from 'react-router-dom';
import {
  AppstoreOutlined,
  AuditOutlined,
  BarChartOutlined,
  BellOutlined,
  CustomerServiceOutlined,
  DatabaseOutlined,
  DeploymentUnitOutlined,
  DownOutlined,
  FileTextOutlined,
  GlobalOutlined,
  LogoutOutlined,
  MenuFoldOutlined,
  MenuUnfoldOutlined,
  SafetyCertificateOutlined,
  SearchOutlined,
  SettingOutlined,
  UserOutlined,
  WalletOutlined,
} from '@ant-design/icons';
import { Avatar, Badge, Button, Dropdown, Input, Layout, Menu, Result } from 'antd';
import { clearSession, getRole, getToken } from './api';
import { canAccess, firstModuleKey, MODULES, roleLabel, visibleModules } from './rbac';

import Login from './pages/Login';
import Users from './pages/Users';
import Audit from './pages/Audit';
import WorldsOps from './pages/WorldsOps';
import Economy from './pages/Economy';
import Metrics from './pages/Metrics';
import Governance from './pages/Governance';
import Risk from './pages/Risk';
import Tickets from './pages/Tickets';

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

const MODULE_ICON: Record<string, ReactNode> = {
  users: <UserOutlined />,
  audit: <AuditOutlined />,
  worlds: <GlobalOutlined />,
  economy: <WalletOutlined />,
  metrics: <BarChartOutlined />,
  prompts: <DeploymentUnitOutlined />,
  risk: <SafetyCertificateOutlined />,
  tickets: <CustomerServiceOutlined />,
};

const PAGE_TITLE: Record<string, string> = {
  users: '用户管理',
  audit: '内容审核',
  worlds: '世界运行监控',
  economy: '经济运营',
  metrics: '数据看板',
  prompts: '模型与 Prompt',
  risk: '风控',
  tickets: '客服与工单',
};

function Forbidden({ moduleLabel }: { moduleLabel?: string }) {
  return (
    <Result
      status="403"
      title="无权限访问"
      subTitle={`当前登录角色无权访问「${moduleLabel ?? '该模块'}」。如需权限请联系超级管理员。`}
    />
  );
}

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
  const [collapsed, setCollapsed] = useState(false);
  const designPreview = (import.meta as any).env?.DEV && new URLSearchParams(location.search).get('design') === 'preview';
  if (!getToken() && !designPreview) return <Navigate to="/login" replace />;

  const role = designPreview ? 'operator' : getRole();
  const visible = visibleModules(role);
  const landing = firstModuleKey(role);
  const active = location.pathname.split('/')[1] || landing || '';
  const withPreview = (path: string) => `${path}${designPreview ? '?design=preview' : ''}`;

  const logout = () => {
    clearSession();
    navigate('/login', { replace: true });
  };

  return (
    <Layout className="admin-shell">
      <Layout.Sider
        width={220}
        collapsedWidth={72}
        collapsed={collapsed}
        theme="light"
        className="admin-shell__sider"
      >
        <button className="admin-brand" type="button" onClick={() => navigate(withPreview('/worlds'))}>
          <img src="/icon.png" alt="" />
          {!collapsed && <strong>MuseAI 后台</strong>}
        </button>

        <Dropdown
          trigger={['click']}
          menu={{
            items: [
              { key: 'role', label: `当前权限：${roleLabel(role)}`, disabled: true },
              { type: 'divider' },
              { key: 'logout', icon: <LogoutOutlined />, label: '退出登录', onClick: logout },
            ],
          }}
        >
          <Button className="admin-role-button" type="text" block>
            <span className="admin-role-button__dot" />
            {!collapsed && <span>{roleLabel(role)}</span>}
            {!collapsed && <DownOutlined />}
          </Button>
        </Dropdown>

        <Menu
          mode="inline"
          inlineCollapsed={collapsed}
          selectedKeys={[active]}
          className="admin-shell__menu"
          items={visible.map((m) => ({
            key: m.key,
            icon: MODULE_ICON[m.key],
            label: <Link to={withPreview(`/${m.key}`)}>{m.label}</Link>,
          }))}
        />

        {!collapsed && role !== 'admin' && (
          <Dropdown
            trigger={['click']}
            menu={{
              items: [
                {
                  key: 'templates',
                  icon: <DatabaseOutlined />,
                  label: '世界模板',
                  onClick: () => navigate(`/worlds?${designPreview ? 'design=preview&' : ''}view=templates`),
                },
                { type: 'divider' },
                { key: 'hint', label: '其他模块随角色权限显示', disabled: true },
              ],
            }}
          >
            <button className="admin-more-modules" type="button">
              <AppstoreOutlined />
              <span>更多模块</span>
              <DownOutlined />
            </button>
          </Dropdown>
        )}

        <div className="admin-shell__sider-footer">
          <Button type="text" icon={<SettingOutlined />} block>{!collapsed && '设置'}</Button>
          <Button type="text" icon={<FileTextOutlined />} block>{!collapsed && '文档中心'}</Button>
          <Button
            type="text"
            icon={collapsed ? <MenuUnfoldOutlined /> : <MenuFoldOutlined />}
            block
            onClick={() => setCollapsed((value) => !value)}
          >
            {!collapsed && '收起'}
          </Button>
        </div>
      </Layout.Sider>

      <Layout className="admin-shell__workspace">
        <Layout.Header className="admin-shell__header">
          <div className="admin-shell__page-title">{PAGE_TITLE[active] ?? '管理后台'}</div>
          <Input
            className="admin-shell__search"
            prefix={<SearchOutlined />}
            suffix={<span>⌘ K</span>}
            placeholder="搜索世界、房间、角色、事件ID…"
            allowClear
            aria-label="搜索后台数据"
          />
          <div className="admin-shell__account">
            <Button className="admin-environment" type="text">
              <span className="admin-environment__dot" />
              生产环境
              <DownOutlined />
            </Button>
            <Badge count={12} size="small">
              <Button type="text" className="admin-notification" icon={<BellOutlined />} aria-label="通知" />
            </Badge>
            <Button type="text" className="admin-profile">
              <Avatar size={34} src="/assets/worlds/mist-sea-world.png" />
              <span>
                <strong>林逸</strong>
                <small>{roleLabel(role)}</small>
              </span>
              <DownOutlined />
            </Button>
          </div>
        </Layout.Header>

        <Layout.Content className="admin-shell__content">
          <Routes>
            <Route index element={landing ? <Navigate to={withPreview(`/${landing}`)} replace /> : <NoModules />} />
            {MODULES.map((m) => {
              const Page = PAGES[m.key];
              return (
                <Route
                  key={m.key}
                  path={`/${m.key}`}
                  element={canAccess(role, m.key) ? <Page /> : <Forbidden moduleLabel={m.label} />}
                />
              );
            })}
            <Route path="*" element={landing ? <Navigate to={withPreview(`/${landing}`)} replace /> : <NoModules />} />
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
