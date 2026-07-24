// 世界大厅（C1，规格 §2.1）：房型筛选 + 标题搜索 + 最新/热门排序 + 世界卡列表 + 我的世界（未读日报角标）。
// P4a 仅放置房；其余房型标注「未开放」（§2.1 不展示未来空能力）。云端不可用时优雅降级。
import React, { useEffect, useState } from 'react';
import {
  Row,
  Col,
  Card,
  Tag,
  Segmented,
  Typography,
  Button,
  Alert,
  Spin,
  Empty,
  Badge,
  Space,
  Divider,
  Input,
} from 'antd';
import {
  GlobalOutlined,
  TeamOutlined,
  ThunderboltOutlined,
  EyeOutlined,
  RightOutlined,
  FireOutlined,
} from '@ant-design/icons';
import { useNavigate } from 'react-router-dom';
import {
  usePlatformStore,
  roomTypeLabel,
  type WorldSummary,
  type RoomTypeFilter,
  type WorldsSort,
} from '../../stores/usePlatformStore';

const { Title, Text } = Typography;

const ROOM_OPTIONS = [
  { label: '放置房', value: 'idle' as RoomTypeFilter },
  { label: '章节房（未开放）', value: 'chapter' as RoomTypeFilter, disabled: true },
  { label: '赛事房（未开放）', value: 'arena' as RoomTypeFilter, disabled: true },
];

const SORT_OPTIONS = [
  { label: '最新', value: 'new' as WorldsSort },
  { label: '热门', value: 'hot' as WorldsSort },
];

const statusMeta = (status: string): { label: string; color: string } => {
  switch (status) {
    case 'open':
      return { label: '开放中', color: 'green' };
    case 'running':
      return { label: '运行中', color: 'blue' };
    case 'paused':
      return { label: '已暂停', color: 'orange' };
    default:
      return { label: status, color: 'default' };
  }
};

const WorldCard: React.FC<{ world: WorldSummary; onEnter: () => void; onSpectate: () => void }> = ({
  world,
  onEnter,
  onSpectate,
}) => {
  const sm = statusMeta(world.status);
  return (
    <Card
      hoverable
      onClick={onEnter}
      style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)', height: '100%' }}
      styles={{ body: { padding: 20 } }}
    >
      <Space direction="vertical" size={10} style={{ width: '100%' }}>
        <Space style={{ justifyContent: 'space-between', width: '100%' }} align="start">
          <Text strong style={{ fontSize: 16, color: '#33312e' }}>
            {world.title}
          </Text>
          <Tag color={sm.color}>{sm.label}</Tag>
        </Space>
        <Space size={6} wrap>
          <Tag color="orange">{roomTypeLabel(world.roomType)}</Tag>
          {/* 星级（1-5）：星级≥3 的世界对投放角色有历练门槛，与热度徽标并列。 */}
          {typeof world.starRating === 'number' && <Tag color="gold">{world.starRating}★</Tag>}
          {world.aiLabel?.visible !== false && <Tag>AI 生成</Tag>}
          {typeof world.hotScore === 'number' && (
            <Tag color="volcano">
              <FireOutlined /> 热度 {world.hotScore}
            </Tag>
          )}
        </Space>
        <Space size={20} style={{ color: '#8c857b', fontSize: 13 }}>
          <span>
            <TeamOutlined /> {world.memberCount}/{world.memberLimit} 角色
          </span>
          <span>
            <ThunderboltOutlined /> 每日 {world.tickPerDay} 拍
          </span>
        </Space>
        <Space size={8} style={{ marginTop: 4 }}>
          <Button
            type="primary"
            size="small"
            onClick={(e) => {
              e.stopPropagation();
              onEnter();
            }}
          >
            进入世界
          </Button>
          <Button
            size="small"
            icon={<EyeOutlined />}
            onClick={(e) => {
              e.stopPropagation();
              onSpectate();
            }}
          >
            观战
          </Button>
        </Space>
      </Space>
    </Card>
  );
};

const PlatformHall: React.FC = () => {
  const navigate = useNavigate();
  const {
    roomTypeFilter,
    worldsQuery,
    worldsSort,
    worlds,
    worldsLoading,
    worldsError,
    worldsHasMore,
    myWorlds,
    worldTitles,
    setRoomTypeFilter,
    setWorldsQuery,
    setWorldsSort,
    loadWorlds,
    loadReports,
  } = usePlatformStore();

  // 搜索框受控值：初值取 store（跨导航保留已生效的搜索词），仅回车/点按/清空时才提交请求。
  const [searchText, setSearchText] = useState(worldsQuery);

  useEffect(() => {
    void loadWorlds(true);
    void loadReports();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const unreadWorlds = myWorlds.filter((w) => w.unreadCount > 0);

  return (
    <div style={{ padding: '32px 40px', maxWidth: 1180, margin: '0 auto' }}>
      <div style={{ marginBottom: 24 }}>
        <Title level={2} style={{ margin: 0, color: '#33312e', fontWeight: 500 }}>
          <GlobalOutlined style={{ color: '#d97757', marginRight: 10 }} />
          世界大厅
        </Title>
        <Text type="secondary">选一个精选世界，把你的角色投进去，看它替你活、替你结缘。</Text>
      </div>

      {/* 我的世界（有未读日报优先提示） */}
      {myWorlds.length > 0 && (
        <Card
          size="small"
          style={{ marginBottom: 20, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.04)' }}
          styles={{ body: { padding: 16 } }}
        >
          <Space style={{ justifyContent: 'space-between', width: '100%' }}>
            <Text strong style={{ color: '#8c857b' }}>
              我的世界
            </Text>
            <Button type="link" size="small" onClick={() => navigate('/platform/my')}>
              查看全部 <RightOutlined />
            </Button>
          </Space>
          <Divider style={{ margin: '10px 0' }} />
          <Space size={12} wrap>
            {myWorlds.slice(0, 4).map((w) => (
              <Badge key={w.worldId} count={w.unreadCount} size="small" offset={[-4, 4]}>
                <Button onClick={() => navigate(`/platform/worlds/${w.worldId}`)}>
                  {worldTitles[w.worldId] || w.worldId} · {w.characterIds.length} 角色
                </Button>
              </Badge>
            ))}
          </Space>
          {unreadWorlds.length > 0 && (
            <Alert
              type="success"
              showIcon
              style={{ marginTop: 12 }}
              message={`有 ${unreadWorlds.reduce((n, w) => n + w.unreadCount, 0)} 份未读日报，去看看你的角色昨天做了什么`}
              action={
                <Button size="small" type="text" onClick={() => navigate('/platform/reports')}>
                  阅读日报
                </Button>
              }
            />
          )}
        </Card>
      )}

      <Space size={12} wrap style={{ marginBottom: 20, width: '100%', justifyContent: 'space-between' }}>
        <Segmented
          options={ROOM_OPTIONS}
          value={roomTypeFilter}
          onChange={(v) => void setRoomTypeFilter(v as RoomTypeFilter)}
        />
        <Space size={12} wrap>
          <Input.Search
            allowClear
            placeholder="搜索世界标题"
            value={searchText}
            style={{ width: 240 }}
            onChange={(e) => {
              const v = e.target.value;
              setSearchText(v);
              // 清空（点 × 或删光）即恢复未搜索列表；键入过程不发请求。
              if (v === '' && worldsQuery !== '') void setWorldsQuery('');
            }}
            onSearch={(v) => void setWorldsQuery(v)}
          />
          <Segmented
            options={SORT_OPTIONS}
            value={worldsSort}
            onChange={(v) => void setWorldsSort(v as WorldsSort)}
          />
        </Space>
      </Space>

      {worldsError ? (
        <Alert
          type="error"
          showIcon
          message="连接平台失败"
          description={worldsError}
          action={
            <Button size="small" onClick={() => void loadWorlds(true)}>
              重试
            </Button>
          }
        />
      ) : worldsLoading && worlds.length === 0 ? (
        <div style={{ textAlign: 'center', padding: 60 }}>
          <Spin />
        </div>
      ) : worlds.length === 0 ? (
        <Empty
          description={worldsQuery ? '没有匹配的世界，换个关键词试试' : '暂无开放世界，稍后再来看看'}
          style={{ padding: 60 }}
        />
      ) : (
        <>
          <Row gutter={[16, 16]}>
            {worlds.map((w) => (
              <Col key={w.id} xs={24} sm={12} lg={8}>
                <WorldCard
                  world={w}
                  onEnter={() => navigate(`/platform/worlds/${w.id}`)}
                  onSpectate={() => navigate(`/platform/worlds/${w.id}/spectate`)}
                />
              </Col>
            ))}
          </Row>
          {/* 热门是快照榜不分页：即便切换瞬间残留 hasMore 也不展示加载更多 */}
          {worldsSort !== 'hot' && worldsHasMore && (
            <div style={{ textAlign: 'center', marginTop: 24 }}>
              <Button loading={worldsLoading} onClick={() => void loadWorlds(false)}>
                加载更多
              </Button>
            </div>
          )}
        </>
      )}
    </div>
  );
};

export default PlatformHall;
