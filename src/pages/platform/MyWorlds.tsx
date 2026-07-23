// 我的世界（C1，规格 §2.1）：已投放角色的世界列表 + 未读日报角标。
// 数据来源：/me/reports 按世界聚合（无独立 memberships 列表端点，用日报反推投放世界）。
import React, { useEffect } from 'react';
import { Typography, List, Card, Badge, Button, Tag, Alert, Spin, Empty, Space } from 'antd';
import { AppstoreOutlined, TeamOutlined, ReadOutlined, EyeOutlined } from '@ant-design/icons';
import { useNavigate } from 'react-router-dom';
import { usePlatformStore } from '../../stores/usePlatformStore';

const { Title, Text } = Typography;

const MyWorlds: React.FC = () => {
  const navigate = useNavigate();
  const { myWorlds, worldTitles, reportsLoading, reportsError, loadReports } = usePlatformStore();

  useEffect(() => {
    void loadReports();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div style={{ padding: '32px 40px', maxWidth: 900, margin: '0 auto' }}>
      <div style={{ marginBottom: 24 }}>
        <Title level={2} style={{ margin: 0, color: '#33312e', fontWeight: 500 }}>
          <AppstoreOutlined style={{ color: '#d97757', marginRight: 10 }} />
          我的世界
        </Title>
        <Text type="secondary">你已投放角色的世界，以及等待阅读的角色日报。</Text>
      </div>

      {reportsError ? (
        <Alert
          type="error"
          showIcon
          message="连接平台失败"
          description={reportsError}
          action={
            <Button size="small" onClick={() => void loadReports()}>
              重试
            </Button>
          }
        />
      ) : reportsLoading && myWorlds.length === 0 ? (
        <div style={{ textAlign: 'center', padding: 60 }}>
          <Spin />
        </div>
      ) : myWorlds.length === 0 ? (
        <Empty description="你还没有把角色投进任何世界" style={{ padding: 60 }}>
          <Button type="primary" onClick={() => navigate('/platform')}>
            去大厅投放角色
          </Button>
        </Empty>
      ) : (
        <List
          dataSource={myWorlds}
          rowKey={(w) => w.worldId}
          renderItem={(w) => (
            <Card
              style={{ marginBottom: 12, borderRadius: 12, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.05)' }}
              styles={{ body: { padding: 18 } }}
            >
              <Space style={{ justifyContent: 'space-between', width: '100%' }} align="start">
                <Space direction="vertical" size={6}>
                  <Space size={10}>
                    <Text strong style={{ fontSize: 16, color: '#33312e' }}>
                      {worldTitles[w.worldId] || w.worldId}
                    </Text>
                    {w.unreadCount > 0 && <Badge count={w.unreadCount} />}
                  </Space>
                  <Space size={16} style={{ color: '#8c857b', fontSize: 13 }}>
                    <span>
                      <TeamOutlined /> {w.characterIds.length} 个角色
                    </span>
                    <span>共 {w.totalReports} 份日报</span>
                    {w.latestReportDay && <Tag>最近：{w.latestReportDay}</Tag>}
                  </Space>
                </Space>
                <Space direction="vertical" size={8} align="end">
                  <Button type="primary" onClick={() => navigate(`/platform/worlds/${w.worldId}`)}>
                    进入世界
                  </Button>
                  <Space size={6}>
                    <Button
                      size="small"
                      icon={<ReadOutlined />}
                      disabled={!w.latestReportId}
                      onClick={() => w.latestReportId && navigate(`/platform/reports/${w.latestReportId}`)}
                    >
                      最新日报
                    </Button>
                    <Button
                      size="small"
                      icon={<EyeOutlined />}
                      onClick={() => navigate(`/platform/worlds/${w.worldId}/spectate`)}
                    >
                      观战
                    </Button>
                  </Space>
                </Space>
              </Space>
            </Card>
          )}
        />
      )}
    </div>
  );
};

export default MyWorlds;
