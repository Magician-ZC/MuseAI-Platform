// 管理后台外壳：角色收敛菜单 + 与客户端一致的 MuseAI 暖色视觉系统。
import { lazy, Suspense, useEffect, useState, type ComponentType, type ReactNode } from 'react';
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
  FlagOutlined,
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
import { Avatar, Badge, Button, Dropdown, Input, Layout, Menu, message, Result, Spin } from 'antd';
import { adminFetch, clearSession, getRole, getToken, resolveEnvironment } from './api';
import { canAccess, firstModuleKey, MODULES, roleLabel, visibleModules } from './rbac';
import { resolveSearch } from './globalSearch';

// 🔴 Login 保持静态：它是**未登录时唯一能到达的页面**，懒加载它等于让登录页自己也要等一次下载。
import Login from './pages/Login';

// 九个业务模块按路由分包（2026-07-29）。实测入口 chunk 2,512,317 B → 826,150 B（-67.1%，
// gzip 823,006 → 267,193 B）：echarts + zrender 占 admin 源码字节的 53.3%，而九个模块里
// 只有经济 / 数据看板 / 世界运营三个真的用得上它——其余六个模块的运营此前每次打开后台
// 都在为一个自己永远不会看到的图表库付启动成本。
const Users = lazy(() => import('./pages/Users'));
const Audit = lazy(() => import('./pages/Audit'));
const WorldsOps = lazy(() => import('./pages/WorldsOps'));
const Economy = lazy(() => import('./pages/Economy'));
const Metrics = lazy(() => import('./pages/Metrics'));
const Governance = lazy(() => import('./pages/Governance'));
const Risk = lazy(() => import('./pages/Risk'));
const SocialReports = lazy(() => import('./pages/SocialReports'));
const Tickets = lazy(() => import('./pages/Tickets'));

const PAGES: Record<string, ComponentType> = {
  users: Users,
  audit: Audit,
  worlds: WorldsOps,
  economy: Economy,
  metrics: Metrics,
  prompts: Governance,
  risk: Risk,
  social: SocialReports,
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
  social: <FlagOutlined />,
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
  social: '社交举报',
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

/** `GET /admin/me` 的响应。**没有** avatarUrl/permissions——服务端刻意不下发，见那个 handler 的注释。 */
interface AdminMe {
  userId: string;
  role: string;
  nickname: string;
  status: string;
}

/** `GET /admin/me/pending` 的响应：只含**这个角色能处置**的队列。 */
interface PendingQueues {
  total: number;
  queues: { key: string; label: string; module: string; count: number }[];
}

function Shell() {
  const location = useLocation();
  const navigate = useNavigate();
  const [collapsed, setCollapsed] = useState(false);
  // 右上角的两条「接口缺字段」此前恒为占位：铃铛红点写死 0、账号名只显示角色。
  // 现在各自有了真数据源（`GET /admin/me` / `GET /admin/me/pending`）。
  // 🔴 拿不到就**退回占位**而不是报错：外壳挂了会让整个后台不可用，
  // 而这两项都只是身份显示与待办提示，够不上让人登不进来。
  const [me, setMe] = useState<AdminMe | null>(null);
  const [pending, setPending] = useState<PendingQueues | null>(null);

  useEffect(() => {
    if (!getToken()) return;
    let cancelled = false;
    void (async () => {
      try {
        const [m, p] = await Promise.all([
          adminFetch<AdminMe>('/admin/me'),
          adminFetch<PendingQueues>('/admin/me/pending'),
        ]);
        if (!cancelled) {
          setMe(m);
          setPending(p);
        }
      } catch {
        // 静默：见上。
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const designPreview = (import.meta as any).env?.DEV && new URLSearchParams(location.search).get('design') === 'preview';
  if (!getToken() && !designPreview) return <Navigate to="/login" replace />;

  const role = designPreview ? 'operator' : getRole();
  // 设计文档 §8：环境标识必须来自真实来源（构建注入 / 接口基址），判不出来显示「环境未知」。
  const environment = resolveEnvironment();
  const visible = visibleModules(role);
  const landing = firstModuleKey(role);
  const active = location.pathname.split('/')[1] || landing || '';
  const withPreview = (path: string) => `${path}${designPreview ? '?design=preview' : ''}`;
  // 🔴 只列 count > 0 的队列：把一排 0 摆出来，真正有事的那条就看不见了。
  const pendingQueues = (pending?.queues ?? []).filter((q) => q.count > 0);
  const pendingTotal = pending?.total ?? 0;

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
                  label: '人工校准（阶段 / 身份池 / 境界档）',
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
          {/* 🔴 这是**分发**不是检索，placeholder 也照实改了：后台今天只有 users 一个端点
              支持文本检索，其余模块只能按 id 前缀把人送到对的地方（见 globalSearch.ts）。
              认不出来时给一句「没认出来」，而不是静默——静默会让它长期看起来像已经接好了。 */}
          <Input.Search
            className="admin-shell__search"
            prefix={<SearchOutlined />}
            placeholder="粘贴 ID 直达（wld_/tpl_/cchar_/srp_…），或输入手机号 / 邮箱查账号"
            allowClear
            aria-label="搜索后台数据"
            onSearch={(value) => {
              const hit = resolveSearch(value);
              if (!hit) {
                if (value.trim()) message.info('没认出这个标识。可粘贴主体 ID（形如 wld_xxx），或输入手机号 / 邮箱查账号。');
                return;
              }
              // 分发前先过 RBAC：把 support 送进世界运营页只会得到一个 403 结果页，
              // 那比不给搜更糟——他会以为是自己搜错了。
              if (!canAccess(role, hit.module)) {
                message.warning(`这是${hit.what}，归「${MODULES.find((m) => m.key === hit.module)?.label ?? hit.module}」模块，当前角色无权访问。`);
                return;
              }
              const [p, q] = hit.path.split('?');
              const query = new URLSearchParams(q);
              if (designPreview) query.set('design', 'preview');
              const suffix = query.toString();
              navigate(`${p}${suffix ? `?${suffix}` : ''}`);
            }}
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
            {/* 铃铛 = 「有什么在等我」。后台从来没有「通知」这个概念（notification_outbox 是玩家侧的），
                所以它读的是 GET /admin/me/pending —— 只含**这个角色能处置**的队列。
                点开逐条列出并可直接跳到对应模块；一条都没有时不画红点，也不给一个点不动的按钮。 */}
            <Dropdown
              trigger={['click']}
              menu={{
                items:
                  pendingQueues.length > 0
                    ? pendingQueues.map((q) => ({
                        key: q.key,
                        label: `${q.label}：${q.count}`,
                        onClick: () => navigate(withPreview(`/${q.module}`)),
                      }))
                    : [{ key: 'empty', label: '没有待你处理的条目', disabled: true }],
              }}
            >
              <Badge count={designPreview ? 12 : pendingTotal} size="small">
                <Button type="text" className="admin-notification" icon={<BellOutlined />} aria-label="待处理" />
              </Badge>
            </Dropdown>
            {/* 账号名取自 GET /admin/me 的 nickname。⚠️ 头像仍是图标占位——users 表没有头像列，
                造一个空 avatarUrl 下发只会让人以为「接上了但没数据」。要加得先加列，那是产品决定。 */}
            <Button type="text" className="admin-profile">
              {designPreview
                ? <Avatar size={34} src="/assets/worlds/mist-sea-world.png" />
                : <Avatar size={34} icon={<UserOutlined />} />}
              <span>
                <strong>{designPreview ? '林逸' : me?.nickname || roleLabel(role)}</strong>
                <small>{designPreview || me?.nickname ? roleLabel(role) : '当前登录角色'}</small>
              </span>
              <DownOutlined />
            </Button>
          </div>
        </Layout.Header>

        <Layout.Content className="admin-shell__content">
          {/* Suspense 放在内容区**里面**：切模块时左侧菜单与顶栏保持不动，只有内容区转圈。 */}
          <Suspense
            fallback={
              <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 64 }}>
                <Spin />
              </div>
            }
          >
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
          </Suspense>
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
