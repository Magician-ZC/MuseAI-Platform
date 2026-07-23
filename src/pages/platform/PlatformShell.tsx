// 平台模式外壳（C1）：独立路由组 /platform/* 的布局与鉴权门。
// Local-first 红线：平台是独立壳，不复用本地 AppShell，不给任何本地页面加登录门；
// 未登录访问受保护平台页 → 友好引导登录（不劫持、不强制跳转本地）。
import React from 'react';
import { Layout, Menu, ConfigProvider, Typography, Space, Tag, Button, Dropdown } from 'antd';
import {
  GlobalOutlined,
  CloudUploadOutlined,
  AppstoreOutlined,
  ReadOutlined,
  WalletOutlined,
  UserOutlined,
  LogoutOutlined,
  RollbackOutlined,
} from '@ant-design/icons';
import { Outlet, useNavigate, useLocation, Navigate } from 'react-router-dom';
import { warmMinimalistTheme } from '../../theme';
import { useAuthStore } from '../../stores/useAuthStore';
import { cloudFetch } from '../../utils/cloudApi';

const { Header, Content } = Layout;
const { Text } = Typography;

const NAV_ITEMS = [
  { key: '/platform', icon: <GlobalOutlined />, label: '世界大厅' },
  { key: '/platform/publish', icon: <CloudUploadOutlined />, label: '发布角色' },
  { key: '/platform/my', icon: <AppstoreOutlined />, label: '我的世界' },
  { key: '/platform/reports', icon: <ReadOutlined />, label: '日报' },
  { key: '/platform/wallet', icon: <WalletOutlined />, label: '钱包' },
];

/** 当前高亮的导航项：取匹配的最长前缀。 */
function activeNavKey(pathname: string): string {
  const matched = NAV_ITEMS.map((i) => i.key)
    .filter((key) => pathname === key || pathname.startsWith(`${key}/`))
    .sort((a, b) => b.length - a.length);
  return matched[0] ?? '/platform';
}

export const PlatformShell: React.FC = () => {
  const navigate = useNavigate();
  const location = useLocation();
  const user = useAuthStore((s) => s.user);
  const isAuthed = useAuthStore((s) => s.isAuthed());
  const logout = useAuthStore((s) => s.logout);

  const handleLogout = async () => {
    try {
      // 服务端吊销 refresh（best-effort，失败也要本地登出）。
      await cloudFetch('/api/auth/logout', { method: 'POST', idempotent: true });
    } catch {
      // 忽略：本地登出必须成功
    }
    logout();
    navigate('/platform/login');
  };

  return (
    <ConfigProvider theme={warmMinimalistTheme}>
      <Layout style={{ minHeight: '100vh', background: '#faf9f5' }}>
        <Header
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 24,
            background: '#faf9f5',
            borderBottom: '1px solid #eae6df',
            paddingInline: 24,
          }}
        >
          <Space size={10} style={{ flex: '0 0 auto' }}>
            <GlobalOutlined style={{ fontSize: 20, color: '#d97757' }} />
            <Text strong style={{ fontSize: 16, color: '#33312e' }}>
              平台世界
            </Text>
            <Tag color="orange" style={{ marginInlineStart: 4 }}>
              AI 生成内容
            </Tag>
          </Space>

          <Menu
            mode="horizontal"
            selectedKeys={[activeNavKey(location.pathname)]}
            onClick={({ key }) => navigate(key)}
            items={NAV_ITEMS}
            style={{ flex: 1, background: 'transparent', borderBottom: 'none', minWidth: 0 }}
          />

          <Space size={12} style={{ flex: '0 0 auto' }}>
            <Button
              type="text"
              size="small"
              icon={<RollbackOutlined />}
              onClick={() => navigate('/')}
              style={{ color: '#8c857b' }}
            >
              返回本地模式
            </Button>
            {isAuthed ? (
              <Dropdown
                menu={{
                  items: [
                    { key: 'logout', icon: <LogoutOutlined />, label: '退出登录', onClick: handleLogout },
                  ],
                }}
              >
                <Button type="text" icon={<UserOutlined />} style={{ color: '#33312e' }}>
                  {user?.nickname || user?.phone || '已登录'}
                </Button>
              </Dropdown>
            ) : (
              <Button type="primary" size="small" onClick={() => navigate('/platform/login')}>
                登录
              </Button>
            )}
          </Space>
        </Header>

        <Content style={{ background: '#faf9f5', overflow: 'auto' }}>
          <Outlet />
        </Content>
      </Layout>
    </ConfigProvider>
  );
};

/**
 * 受保护平台页的鉴权门：未登录时展示引导（而非静默重定向），
 * 明确告知本地能力无需登录（Local-first）。
 */
export const RequireAuth: React.FC<{ children: React.ReactElement }> = ({ children }) => {
  const isAuthed = useAuthStore((s) => s.isAuthed());
  const location = useLocation();
  if (!isAuthed) {
    return <Navigate to="/platform/login" replace state={{ from: location.pathname }} />;
  }
  return children;
};

export default PlatformShell;
