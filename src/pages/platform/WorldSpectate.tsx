// 观战席（C1，规格 §2.7 / §11）：只读事件流，无干预与同意面板。
// 复用 WorldRoom 导出的事件订阅与 L1 视图组件；观战资格由服务端 can_view_world 校验（public/official 世界开放）。
import React, { useEffect, useState } from 'react';
import { Alert, Button, Space, Spin } from 'antd';
import { useParams, useNavigate } from 'react-router-dom';
import { cloudFetch } from '../../utils/cloudApi';
import { usePlatformStore, describeCloudError, type WorldDetail } from '../../stores/usePlatformStore';
import { useWorldEvents, WorldHeader, WorldViewPanel } from './WorldRoom';

const WorldSpectate: React.FC = () => {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const roomView = usePlatformStore((s) => s.roomView);
  const setRoomView = usePlatformStore((s) => s.setRoomView);

  const [world, setWorld] = useState<WorldDetail | null>(null);
  const [worldError, setWorldError] = useState<string | null>(null);
  const [worldLoading, setWorldLoading] = useState(true);

  const { events, loading: eventsLoading, error: eventsError } = useWorldEvents(id);

  const loadWorld = async () => {
    if (!id) return;
    setWorldLoading(true);
    setWorldError(null);
    try {
      const d = await cloudFetch<WorldDetail>(`/api/worlds/${id}`);
      setWorld(d);
    } catch (e) {
      setWorldError(describeCloudError(e));
    } finally {
      setWorldLoading(false);
    }
  };

  useEffect(() => {
    void loadWorld();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  if (worldLoading && !world) {
    return (
      <div style={{ textAlign: 'center', padding: 80 }}>
        <Spin />
      </div>
    );
  }

  if (worldError && !world) {
    return (
      <div style={{ padding: '32px 40px', maxWidth: 1180, margin: '0 auto' }}>
        <Alert
          type="error"
          showIcon
          message="连接平台失败"
          description={worldError}
          action={
            <Space>
              <Button size="small" onClick={() => void loadWorld()}>
                重试
              </Button>
              <Button size="small" type="text" onClick={() => navigate('/platform')}>
                返回大厅
              </Button>
            </Space>
          }
        />
      </div>
    );
  }

  if (!world) return null;

  return (
    <div style={{ padding: '24px 40px', maxWidth: 1000, margin: '0 auto' }}>
      <WorldHeader world={world} spectate />
      <WorldViewPanel
        view={roomView}
        onViewChange={setRoomView}
        events={events}
        roster={world.roster}
        loading={eventsLoading}
        error={eventsError}
      />
    </div>
  );
};

export default WorldSpectate;
