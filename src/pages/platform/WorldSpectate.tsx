// 观战席（C1，规格 §2.7 / §11）：只读事件流，无干预与同意面板。
// 复用 WorldRoom 导出的事件订阅与 L1 视图组件；观战资格由服务端 can_view_world 校验（public/official 世界开放）。
// 实时演化：额外订阅 cloudStream，收到 relation/status 类事件后去抖（~2s）重拉权威快照（state-summary），
// 使关系图谱/势力地图/状态面板/时间线随赛况活起来。观众身份下服务端已把 relations 投影为 principal 过滤后的 public 子集，
// 前端不做人工遮罩（防剧透靠服务端投影）。
import React, { useEffect, useRef, useState } from 'react';
import { Alert, Button, Space, Spin } from 'antd';
import { useParams, useNavigate } from 'react-router-dom';
import { cloudFetch, cloudStream } from '../../utils/cloudApi';
import {
  usePlatformStore,
  describeCloudError,
  type WorldDetail,
  type WorldEventItem,
} from '../../stores/usePlatformStore';
import { useWorldEvents, useWorldStateSummary, WorldHeader, WorldViewPanel } from './WorldRoom';

// 触发权威快照重拉的事件类型：关系/状态/仲裁/结盟/冲突/道具（这些会改变 narrative_state）。
const STATE_EVENT_RE = /status|alliance|conflict|arbiter|relation|item/i;
const STATE_RELOAD_DEBOUNCE_MS = 2000;

const WorldSpectate: React.FC = () => {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const roomView = usePlatformStore((s) => s.roomView);
  const setRoomView = usePlatformStore((s) => s.setRoomView);

  const [world, setWorld] = useState<WorldDetail | null>(null);
  const [worldError, setWorldError] = useState<string | null>(null);
  const [worldLoading, setWorldLoading] = useState(true);

  const { events, loading: eventsLoading, error: eventsError } = useWorldEvents(id);
  // 权威快照（观众视角）：驱动关系图谱/势力地图/状态面板/时间线；端点未就绪时组件回退事件启发式。
  const { summary, reload: reloadSummary } = useWorldStateSummary(id);

  // 实时演化：单独订阅世界流，收到 relation/status 类事件后去抖重拉 state-summary（避免每拍狂拉）。
  const reloadRef = useRef(reloadSummary);
  reloadRef.current = reloadSummary;
  useEffect(() => {
    if (!id) return;
    let unsub: (() => void) | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const scheduleReload = () => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => reloadRef.current(), STATE_RELOAD_DEBOUNCE_MS);
    };
    try {
      unsub = cloudStream(
        id,
        (raw) => {
          const ev = raw as WorldEventItem;
          if (!ev || typeof ev.type !== 'string') return;
          if (STATE_EVENT_RE.test(ev.type)) scheduleReload();
        },
        () => {
          // 实时流异常不致命：保留已加载快照，等待 cloudStream 自动重连补偿。
        },
      );
    } catch {
      // WebSocket 不可用（离线等）：降级为静态快照，页面不崩。
    }
    return () => {
      if (timer) clearTimeout(timer);
      if (unsub) unsub();
    };
  }, [id]);

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
        summary={summary}
        loading={eventsLoading}
        error={eventsError}
      />
    </div>
  );
};

export default WorldSpectate;
