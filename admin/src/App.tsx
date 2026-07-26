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
  ExperimentOutlined,
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
import { clearSession, getRole, getToken, resolveEnvironment } from './api';
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
  // 设计文档 §8：环境标识必须来自真实来源（构建注入 / 接口基址），判不出来显示「环境未知」。
  const environment = resolveEnvironment();
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

        {/* 「更多模块」收纳的是**子视图**（世界模板 / 人工校准），不是 RBAC 模块——它们不在
            MODULES 里，因此不会出现在上面的主菜单。原先此块对 admin 隐藏，导致权限最高的角色
            反而没有任何菜单路径进得去这两个页面；子视图与角色无关，故对全部角色显示，
            访问权仍由后端 require_role(operator) 二次校验。 */}
        {!collapsed && (
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
                {
                  key: 'calibration',
                  icon: <ExperimentOutlined />,
                  label: '人工校准（阶段 / 身份池）',
                  onClick: () => navigate(`/worlds?${designPreview ? 'design=preview&' : ''}view=calibration`),
                },
                { type: 'divider' },
                {
                  key: 'hint',
                  label: role === 'admin' ? '以上为世界运营的低频子视图' : '其他模块随角色权限显示',
                  disabled: true,
                },
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
            <Dropdown
              trigger={['click']}
              menu={{
                items: [
                  { key: 'env', label: `环境：${environment.label}`, disabled: true },
                  { key: 'base', label: `接口地址：${environment.apiBase}`, disabled: true },
                  { key: 'mode', label: `构建模式：${environment.buildMode}`, disabled: true },
                  {
                    key: 'hint',
                    label: environment.key === 'unknown'
                      ? '未注入 VITE_ADMIN_ENV，无法判定环境'
                      : '环境标识仅供核对，服务端仍会二次校验权限',
                    disabled: true,
                  },
                ],
              }}
            >
              <Button className="admin-environment" type="text" aria-label={`当前环境：${environment.label}`}>
                <span className={`admin-environment__dot${environment.key === 'production' ? '' : ' is-unverified'}`} />
                {environment.label}
                <DownOutlined />
              </Button>
            </Dropdown>
            {/* TODO(接口缺字段): 未读通知数 —— 后台无通知计数接口，正式模式不展示伪造的红点数字。 */}
            <Badge count={designPreview ? 12 : 0} size="small">
              <Button type="text" className="admin-notification" icon={<BellOutlined />} aria-label="通知" />
            </Badge>
            {/* TODO(接口缺字段): GET /admin/me（displayName / avatarUrl）—— 正式模式只显示已知的登录角色。 */}
            <Button type="text" className="admin-profile">
              {designPreview
                ? <Avatar size={34} src="/assets/worlds/mist-sea-world.png" />
                : <Avatar size={34} icon={<UserOutlined />} />}
              <span>
                <strong>{designPreview ? '林逸' : roleLabel(role)}</strong>
                <small>{designPreview ? roleLabel(role) : '当前登录角色'}</small>
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
