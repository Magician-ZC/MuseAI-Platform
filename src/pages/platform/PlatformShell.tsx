// 平台空间外壳：与本地工作室共享同一客户端身份，但保持独立的信息架构。
import React, { useEffect, useState } from 'react';
import {
  Avatar,
  Button,
  ConfigProvider,
  Drawer,
  Dropdown,
  Input,
  Layout,
  Menu,
  Tooltip,
} from 'antd';
import {
  AppstoreOutlined,
  BellOutlined,
  BookOutlined,
  CompassOutlined,
  DownOutlined,
  GlobalOutlined,
  HeartOutlined,
  HomeOutlined,
  LogoutOutlined,
  MenuOutlined,
  ReadOutlined,
  SearchOutlined,
  SettingOutlined,
  ShoppingOutlined,
  TrophyOutlined,
  WalletOutlined,
} from '@ant-design/icons';
import { Navigate, Outlet, useLocation, useNavigate } from 'react-router-dom';
import RouteFallback from '../../components/RouteFallback';
import { warmMinimalistTheme } from '../../theme';
import { useAuthStore } from '../../stores/useAuthStore';
import { usePlatformStore } from '../../stores/usePlatformStore';
import { cloudFetch } from '../../utils/cloudApi';
import './PlatformShell.css';

const { Header, Sider, Content } = Layout;

const PRIMARY_NAV_ITEMS = [
  { key: '/platform', icon: <GlobalOutlined />, label: '世界大厅' },
  { key: '/platform/my', icon: <BookOutlined />, label: '我的房间' },
  { key: '/platform/worlds/publish', icon: <AppstoreOutlined />, label: '我的发布' },
  { key: '/platform/journey', icon: <CompassOutlined />, label: '我的旅程' },
  { key: '/platform/backpack', icon: <ShoppingOutlined />, label: '背包' },
  { key: '/platform/bonds', icon: <HeartOutlined />, label: '羁绊' },
  {
    key: 'arena',
    icon: <TrophyOutlined />,
    label: <Tooltip title="进入支持赛事的世界后开启">竞技场</Tooltip>,
    disabled: true,
  },
];

const SECONDARY_NAV_ITEMS = [
  { key: '/platform/reports', icon: <ReadOutlined />, label: '世界日报' },
  { key: '/platform/wallet', icon: <WalletOutlined />, label: '钱包' },
];

/** 当前高亮的导航项：取匹配的最长前缀。 */
function activeNavKey(pathname: string): string {
  const keys = [...PRIMARY_NAV_ITEMS, ...SECONDARY_NAV_ITEMS]
    .map((item) => item.key)
    .filter((key) => key.startsWith('/'));
  const matched = keys
    .filter((key) => pathname === key || pathname.startsWith(`${key}/`))
    .sort((a, b) => b.length - a.length);
  if (pathname.startsWith('/platform/worlds/') && pathname !== '/platform/worlds/publish') return '/platform';
  return matched[0] ?? '/platform';
}

export const PlatformShell: React.FC = () => {
  const navigate = useNavigate();
  const location = useLocation();
  const user = useAuthStore((state) => state.user);
  const isAuthed = useAuthStore((state) => state.isAuthed());
  const logout = useAuthStore((state) => state.logout);
  const worldsQuery = usePlatformStore((state) => state.worldsQuery);
  const setWorldsQuery = usePlatformStore((state) => state.setWorldsQuery);
  const [searchText, setSearchText] = useState(worldsQuery);
  const [mobileNavOpen, setMobileNavOpen] = useState(false);
  const isLogin = location.pathname === '/platform/login';
  const designPreview = import.meta.env.DEV && new URLSearchParams(location.search).get('design') === 'preview';
  const showProfile = isAuthed || designPreview;
  const navigatePlatform = (path: string) => {
    setMobileNavOpen(false);
    navigate(designPreview ? `${path}?design=preview` : path);
  };

  useEffect(() => setSearchText(worldsQuery), [worldsQuery]);

  const handleLogout = async () => {
    try {
      await cloudFetch('/api/auth/logout', { method: 'POST', idempotent: true });
    } catch {
      // 服务端不可用时也必须允许退出本地会话。
    }
    logout();
    navigate('/platform/login');
  };

  const submitSearch = async (value: string) => {
    navigate('/platform');
    await setWorldsQuery(value.trim());
  };

  return (
    <ConfigProvider theme={warmMinimalistTheme}>
      <Layout className="platform-shell">
        <Header className="platform-shell__header">
          {!isLogin && (
            <Button
              className="platform-mobile-menu"
              type="text"
              aria-label="打开平台导航"
              icon={<MenuOutlined />}
              onClick={() => setMobileNavOpen(true)}
            />
          )}
          <button className="platform-brand" type="button" onClick={() => navigate('/platform')} aria-label="MuseAI 平台世界首页">
            <img src="/icon.png" alt="" />
            <span>MuseAI</span>
          </button>

          {!isLogin && (
            <Input
              className="platform-global-search"
              value={searchText}
              onChange={(event) => setSearchText(event.target.value)}
              onPressEnter={() => void submitSearch(searchText)}
              onClear={() => void submitSearch('')}
              allowClear
              prefix={<SearchOutlined />}
              suffix={<span className="platform-global-search__hint">⌘ K</span>}
              placeholder="搜索世界、角色、房间、发布…"
              aria-label="搜索平台世界"
            />
          )}

          <div className="platform-account">
            {!isLogin && showProfile && (
              <Button className="platform-icon-button" type="text" aria-label="通知" icon={<BellOutlined />} />
            )}
            {showProfile ? (
              <Dropdown
                trigger={['click']}
                menu={{
                  items: [
                    { key: 'settings', icon: <SettingOutlined />, label: '账户设置', onClick: () => navigate('/settings') },
                    { type: 'divider' },
                    { key: 'logout', icon: <LogoutOutlined />, label: '退出登录', onClick: handleLogout },
                  ],
                }}
              >
                <Button className="platform-profile-button" type="text">
                  <Avatar size={30} src="/assets/characters/kane-night-oath-portrait.png" />
                  <span>{user?.nickname || user?.phone || (designPreview ? '林逸' : '已登录')}</span>
                  <DownOutlined />
                </Button>
              </Dropdown>
            ) : (
              <Button type="primary" onClick={() => navigate('/platform/login')}>登录</Button>
            )}
          </div>
        </Header>

        {isLogin ? (
          <Content className="platform-shell__login-content">
            <RouteFallback>
              <Outlet />
            </RouteFallback>
          </Content>
        ) : (
          <Layout className="platform-shell__body">
            <Sider width={220} theme="light" className="platform-shell__sider">
              <div className="platform-space-panel" aria-label="客户端空间切换">
                <span className="platform-space-panel__label">空间</span>
                <div className="platform-space-switcher">
                  <button type="button" onClick={() => navigate('/')}>
                    <HomeOutlined />
                    <strong>我的工作室</strong>
                  </button>
                  <button type="button" className="is-active" aria-current="page" onClick={() => navigate('/platform')}>
                    <GlobalOutlined />
                    <strong>平台世界</strong>
                  </button>
                </div>
              </div>
            <Menu
                mode="inline"
                selectedKeys={[activeNavKey(location.pathname)]}
                onClick={({ key }) => key.startsWith('/') && navigatePlatform(key)}
                items={PRIMARY_NAV_ITEMS}
                className="platform-shell__menu"
              />
              <div className="platform-shell__sider-footer">
                <Menu
                  mode="inline"
                  selectedKeys={[activeNavKey(location.pathname)]}
                  onClick={({ key }) => navigatePlatform(key)}
                  items={SECONDARY_NAV_ITEMS}
                  className="platform-shell__menu"
                />
                <Button type="text" icon={<SettingOutlined />} onClick={() => navigate('/settings')}>设置</Button>
                <button className="platform-collapse-hint" type="button" aria-label="收起侧栏">
                  <span>‹</span> 收起侧栏
                </button>
              </div>
            </Sider>
            <Content className="platform-shell__content">
              <RouteFallback>
              <Outlet />
            </RouteFallback>
            </Content>
          </Layout>
        )}
        <Drawer
          className="platform-mobile-drawer"
          title="MuseAI 平台世界"
          placement="left"
          size={286}
          open={mobileNavOpen}
          onClose={() => setMobileNavOpen(false)}
        >
          <div className="platform-mobile-drawer__spaces">
            <Button icon={<HomeOutlined />} onClick={() => navigate('/')}>我的工作室</Button>
            <Button type="primary" icon={<GlobalOutlined />} onClick={() => navigatePlatform('/platform')}>平台世界</Button>
          </div>
          <Menu
            mode="inline"
            selectedKeys={[activeNavKey(location.pathname)]}
            onClick={({ key }) => key.startsWith('/') && navigatePlatform(key)}
            items={PRIMARY_NAV_ITEMS}
            className="platform-shell__menu"
          />
          <Menu
            mode="inline"
            selectedKeys={[activeNavKey(location.pathname)]}
            onClick={({ key }) => navigatePlatform(key)}
            items={SECONDARY_NAV_ITEMS}
            className="platform-shell__menu platform-mobile-drawer__secondary"
          />
        </Drawer>
      </Layout>
    </ConfigProvider>
  );
};

/** 未登录访问平台页时，仅跳到平台登录；本地工作室始终保持可用。 */
export const RequireAuth: React.FC<{ children: React.ReactElement }> = ({ children }) => {
  const isAuthed = useAuthStore((state) => state.isAuthed());
  const location = useLocation();
  const designPreview = import.meta.env.DEV && new URLSearchParams(location.search).get('design') === 'preview';
  if (designPreview) return children;
  if (!isAuthed) return <Navigate to="/platform/login" replace state={{ from: location.pathname }} />;
  return children;
};

export default PlatformShell;
