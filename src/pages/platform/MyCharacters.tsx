// 我的角色 · 各世界（C1，规格 §2.1）：以「角色」为轴（与「我的世界」以世界为轴互补）。
// 权威来源 GET /me/memberships（补日报反推盲区：刚投放没日报也在场）；未读日报角标由 /me/reports 派生。
import React, { useEffect, useMemo, useState } from 'react';
import { Typography, Card, Badge, Button, Tag, Alert, Spin, Empty, Space, Popconfirm, message } from 'antd';
import { UserOutlined, ReadOutlined, BranchesOutlined, LoginOutlined, LogoutOutlined } from '@ant-design/icons';
import { useNavigate } from 'react-router-dom';
import { cloudFetch } from '../../utils/cloudApi';
import {
  usePlatformStore,
  describeCloudError,
  roomTypeLabel,
  type Membership,
} from '../../stores/usePlatformStore';

const { Title, Text } = Typography;

/** 世界运行态 → 展示标签。 */
function worldStatusMeta(status: string): { label: string; color: string } {
  switch (status) {
    case 'running':
      return { label: '运行中', color: 'green' };
    case 'open':
      return { label: '开放中', color: 'blue' };
    case 'paused':
      return { label: '已暂停', color: 'gold' };
    case 'ended':
      return { label: '已结束', color: 'default' };
    default:
      return { label: status, color: 'default' };
  }
}

const MyCharacters: React.FC = () => {
  const navigate = useNavigate();
  const {
    memberships,
    membershipsLoading,
    membershipsError,
    loadMemberships,
    reports,
    loadReports,
  } = usePlatformStore();
  const [leaving, setLeaving] = useState<Record<string, boolean>>({});

  const refresh = () => {
    void loadMemberships();
    void loadReports();
  };

  useEffect(() => {
    refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 每（角色 × 世界）的日报统计（未读角标 + 最新日报深链）；reports 已按 createdAt DESC。
  const reportStats = useMemo(() => {
    const m = new Map<string, { unread: number; total: number; latestId?: string }>();
    for (const r of reports) {
      const key = `${r.characterId}__${r.worldId}`;
      const e = m.get(key) ?? { unread: 0, total: 0 };
      e.total += 1;
      if (!r.opened) e.unread += 1;
      if (!e.latestId) e.latestId = r.id;
      m.set(key, e);
    }
    return m;
  }, [reports]);

  // 按角色分组（memberships 已按 joinedAt DESC）。
  const groups = useMemo(() => {
    const byChar = new Map<string, { characterId: string; characterName: string; worlds: Membership[] }>();
    for (const ms of memberships) {
      const g =
        byChar.get(ms.cloudCharacterId) ??
        { characterId: ms.cloudCharacterId, characterName: ms.characterName, worlds: [] };
      g.worlds.push(ms);
      byChar.set(ms.cloudCharacterId, g);
    }
    return [...byChar.values()];
  }, [memberships]);

  const leave = async (worldId: string, cloudCharacterId: string) => {
    const key = `${cloudCharacterId}__${worldId}`;
    setLeaving((s) => ({ ...s, [key]: true }));
    try {
      await cloudFetch(`/api/worlds/${worldId}/leave`, {
        method: 'POST',
        idempotent: true,
        body: { cloudCharacterId },
      });
      message.success('已离场，角色将在下一个节拍退出这个世界');
      refresh();
    } catch (e) {
      message.error(describeCloudError(e));
    } finally {
      setLeaving((s) => ({ ...s, [key]: false }));
    }
  };

  return (
    <div style={{ padding: '32px 40px', maxWidth: 900, margin: '0 auto' }}>
      <div style={{ marginBottom: 24 }}>
        <Title level={2} style={{ margin: 0, color: '#33312e', fontWeight: 500 }}>
          <UserOutlined style={{ color: '#d97757', marginRight: 10 }} />
          我的角色
        </Title>
        <Text type="secondary">你投放到各世界的角色，以及它们在每个世界的近况与日报。</Text>
      </div>

      {membershipsError ? (
        <Alert
          type="error"
          showIcon
          message="连接平台失败"
          description={membershipsError}
          action={
            <Button size="small" onClick={refresh}>
              重试
            </Button>
          }
        />
      ) : membershipsLoading && memberships.length === 0 ? (
        <div style={{ textAlign: 'center', padding: 60 }}>
          <Spin />
        </div>
      ) : groups.length === 0 ? (
        <Empty description="你还没有把角色投进任何世界" style={{ padding: 60 }}>
          <Button type="primary" onClick={() => navigate('/platform')}>
            去大厅投放角色
          </Button>
        </Empty>
      ) : (
        <Space direction="vertical" size={16} style={{ width: '100%' }}>
          {groups.map((g) => (
            <Card
              key={g.characterId}
              style={{ borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
              styles={{ body: { padding: 18 } }}
            >
              <Space style={{ justifyContent: 'space-between', width: '100%', marginBottom: 12 }} align="start">
                <Space size={10}>
                  <Text strong style={{ fontSize: 17, color: '#33312e' }}>
                    {g.characterName || g.characterId}
                  </Text>
                  <Tag>{g.worlds.length} 个世界</Tag>
                </Space>
                <Button
                  size="small"
                  icon={<ReadOutlined />}
                  onClick={() => navigate(`/platform/characters/${g.characterId}`)}
                >
                  一生档案
                </Button>
              </Space>

              <Space direction="vertical" size={10} style={{ width: '100%' }}>
                {g.worlds.map((w) => {
                  const key = `${w.cloudCharacterId}__${w.worldId}`;
                  const stat = reportStats.get(key);
                  const sm = worldStatusMeta(w.worldStatus);
                  return (
                    <div
                      key={w.worldId}
                      style={{
                        display: 'flex',
                        justifyContent: 'space-between',
                        alignItems: 'flex-start',
                        gap: 12,
                        flexWrap: 'wrap',
                        padding: '10px 12px',
                        borderRadius: 8,
                        background: '#faf9f5',
                        border: '1px solid #eae6df',
                      }}
                    >
                      <Space direction="vertical" size={4} style={{ minWidth: 0 }}>
                        <Space size={8} wrap>
                          <Text strong style={{ color: '#33312e' }}>
                            {w.worldTitle || w.worldId}
                          </Text>
                          <Tag color="orange">{roomTypeLabel(w.roomType)}</Tag>
                          <Tag color={sm.color}>{sm.label}</Tag>
                          {stat && stat.unread > 0 && <Badge count={stat.unread} />}
                        </Space>
                        <Text type="secondary" style={{ fontSize: 12 }}>
                          {stat ? `共 ${stat.total} 份日报` : '暂无日报'}
                        </Text>
                      </Space>
                      <Space size={6} wrap>
                        <Button
                          size="small"
                          type="primary"
                          icon={<LoginOutlined />}
                          onClick={() => navigate(`/platform/worlds/${w.worldId}`)}
                        >
                          进入
                        </Button>
                        <Button
                          size="small"
                          icon={<BranchesOutlined />}
                          onClick={() =>
                            navigate(`/platform/worlds/${w.worldId}?character=${w.cloudCharacterId}`)
                          }
                        >
                          世界线
                        </Button>
                        <Button
                          size="small"
                          icon={<ReadOutlined />}
                          disabled={!stat?.latestId}
                          onClick={() => stat?.latestId && navigate(`/platform/reports/${stat.latestId}`)}
                        >
                          最新日报
                        </Button>
                        <Popconfirm
                          title="离开这个世界？"
                          description="角色会在下一个节拍退场；日后可再次投放复活。"
                          okText="确认离场"
                          cancelText="取消"
                          onConfirm={() => leave(w.worldId, w.cloudCharacterId)}
                        >
                          <Button size="small" danger icon={<LogoutOutlined />} loading={leaving[key]}>
                            离场
                          </Button>
                        </Popconfirm>
                      </Space>
                    </div>
                  );
                })}
              </Space>
            </Card>
          ))}
        </Space>
      )}
    </div>
  );
};

export default MyCharacters;
