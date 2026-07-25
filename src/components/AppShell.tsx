import React, { useEffect, useState } from 'react';
import { Avatar, Button, ConfigProvider, Dropdown, Input, Layout, Menu, Modal } from 'antd';
import {
  BellOutlined,
  BookOutlined,
  BranchesOutlined,
  ClearOutlined,
  CompassOutlined,
  DeploymentUnitOutlined,
  DownOutlined,
  ExclamationCircleOutlined,
  GlobalOutlined,
  HeartOutlined,
  HomeOutlined,
  MessageOutlined,
  ProfileOutlined,
  ReadOutlined,
  SearchOutlined,
  SettingOutlined,
} from '@ant-design/icons';
import { Outlet, useLocation, useNavigate } from 'react-router-dom';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { warmMinimalistTheme } from '../theme';
import { useAuthStore } from '../stores/useAuthStore';
import { isTauriHost } from '../utils/runtime';
import '../pages/platform/PlatformShell.css';
import './AppShell.css';

const { Header, Sider, Content } = Layout;

const LOCAL_NAV_ITEMS = [
  { key: '/', icon: <HomeOutlined />, label: '首页' },
  { type: 'divider' as const },
  { key: '/works', icon: <BookOutlined />, label: '作品' },
  { key: '/outline', icon: <ProfileOutlined />, label: '大纲' },
  { key: '/de-ai', icon: <ClearOutlined />, label: '去AI味' },
  { key: '/examples', icon: <ReadOutlined />, label: '范文' },
  { type: 'divider' as const },
  { key: '/background', icon: <GlobalOutlined />, label: '背景' },
  { key: '/chat', icon: <MessageOutlined />, label: '聊天' },
  { key: '/adventure', icon: <CompassOutlined />, label: '冒险' },
  { key: '/bond', icon: <HeartOutlined />, label: '羁绊' },
  { type: 'divider' as const },
  { key: '/book-travel-materials', icon: <BranchesOutlined />, label: '素材' },
  { key: '/story', icon: <DeploymentUnitOutlined />, label: '穿书' },
];

const AppShell: React.FC = () => {
  const [permissionRequest, setPermissionRequest] = useState<{ requestId: string; command: string } | null>(null);
  const [searchText, setSearchText] = useState('');
  const navigate = useNavigate();
  const location = useLocation();
  const user = useAuthStore((state) => state.user);
  const isAuthed = useAuthStore((state) => state.isAuthed());

  useEffect(() => {
    if (!isTauriHost()) return;

    let unlistenFn: (() => void) | undefined;
    listen<{ requestId: string; command: string }>('bash-permission-request', (event) => {
      setPermissionRequest(event.payload);
    }).then((fn) => {
      unlistenFn = fn;
    });

    return () => {
      if (unlistenFn) unlistenFn();
    };
  }, []);

  const resolvePermission = async (approved: boolean) => {
    if (!permissionRequest) return;
    try {
      await invoke('resolve_bash_permission', { requestId: permissionRequest.requestId, approved });
    } catch (error) {
      console.error('Failed to resolve bash permission:', error);
    }
    setPermissionRequest(null);
  };

  const submitLocalSearch = () => {
    if (!searchText.trim()) return;
    navigate('/works', { state: { query: searchText.trim() } });
  };

  return (
    <ConfigProvider theme={warmMinimalistTheme}>
      <Layout className="platform-shell local-client-shell">
        <Header className="platform-shell__header">
          <button className="platform-brand" type="button" onClick={() => navigate('/')} aria-label="MuseAI 工作室首页">
            <img src="/icon.png" alt="" />
            <span>MuseAI</span>
          </button>

          <Input
            className="platform-global-search"
            value={searchText}
            onChange={(event) => setSearchText(event.target.value)}
            onPressEnter={submitLocalSearch}
            onClear={() => setSearchText('')}
            allowClear
            prefix={<SearchOutlined />}
            suffix={<span className="platform-global-search__hint">⌘ K</span>}
            placeholder="搜索本地世界、角色、作品…"
            aria-label="搜索我的工作室"
          />

          <div className="platform-account">
            {isAuthed && <Button className="platform-icon-button" type="text" aria-label="通知" icon={<BellOutlined />} />}
            <Dropdown
              trigger={['click']}
              menu={{
                items: [
                  { key: 'settings', icon: <SettingOutlined />, label: '工作室设置', onClick: () => navigate('/settings') },
                  { key: 'platform', icon: <GlobalOutlined />, label: '进入平台世界', onClick: () => navigate('/platform') },
                ],
              }}
            >
              <Button className="platform-profile-button" type="text">
                <Avatar size={30} src="/icon.png" />
                <span>{user?.nickname || user?.phone || '本地工作区'}</span>
                <DownOutlined />
              </Button>
            </Dropdown>
          </div>
        </Header>

        <Layout className="platform-shell__body">
          <Sider width={220} theme="light" className="platform-shell__sider local-client-shell__sider">
            <div className="platform-space-panel" aria-label="客户端空间切换">
              <span className="platform-space-panel__label">空间</span>
              <div className="platform-space-switcher">
                <button type="button" className="is-active" aria-current="page" onClick={() => navigate('/')}>
                  <HomeOutlined />
                  <strong>我的工作室</strong>
                </button>
                <button type="button" onClick={() => navigate('/platform')}>
                  <GlobalOutlined />
                  <strong>平台世界</strong>
                </button>
              </div>
            </div>
            <Menu
              mode="inline"
              selectedKeys={[location.pathname]}
              onClick={({ key }) => navigate(key)}
              items={LOCAL_NAV_ITEMS}
              className="platform-shell__menu local-client-shell__menu"
            />
            <div className="platform-shell__sider-footer">
              <Button type="text" icon={<SettingOutlined />} onClick={() => navigate('/settings')}>设置</Button>
              <button className="platform-collapse-hint" type="button" aria-label="收起侧栏">
                <span>‹</span> 收起侧栏
              </button>
            </div>
          </Sider>
          <Content className="local-client-shell__content">
            <Outlet />
          </Content>
        </Layout>
      </Layout>

      <Modal
        title={
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <ExclamationCircleOutlined style={{ color: '#faad14' }} />
            <span>执行命令请求</span>
          </div>
        }
        open={!!permissionRequest}
        closable={false}
        mask={{ closable: false }}
        footer={[
          <Button key="deny" onClick={() => void resolvePermission(false)}>拒绝</Button>,
          <Button key="approve" type="primary" danger onClick={() => void resolvePermission(true)}>允许执行</Button>,
        ]}
      >
        <p>AI 助手请求执行以下终端命令，是否允许？</p>
        <div className="local-client-shell__permission-command">{permissionRequest?.command}</div>
      </Modal>
    </ConfigProvider>
  );
};

export default AppShell;
