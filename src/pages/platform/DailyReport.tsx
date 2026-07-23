// 日报阅读态（C1，规格 §2.5）：高光卡 + 关系变化 + 角色独白；明确区分公开事实/私密视角/模型推断。
// 打开详情即 GET /me/reports/{id}（服务端回写 opened_at = P4a 北极星埋点）。无 :id 时展示日报列表。
import React, { useEffect, useState } from 'react';
import { Typography, Card, Tag, Space, Alert, Spin, Empty, Button, List, Badge, Divider, Tooltip } from 'antd';
import { ReadOutlined, LeftOutlined, MessageOutlined } from '@ant-design/icons';
import { useParams, useNavigate } from 'react-router-dom';
import { cloudFetch } from '../../utils/cloudApi';
import {
  usePlatformStore,
  describeCloudError,
  provenanceMeta,
  eventTypeMeta,
  type ReportDetail,
  type ReportHighlight,
} from '../../stores/usePlatformStore';

const { Title, Text, Paragraph } = Typography;

const ProvenanceTag: React.FC<{ kind: string }> = ({ kind }) => {
  const meta = provenanceMeta(kind);
  return (
    <Tooltip title={meta.hint}>
      <Tag color={meta.color}>{meta.label}</Tag>
    </Tooltip>
  );
};

const HighlightCard: React.FC<{ item: ReportHighlight }> = ({ item }) => {
  const tm = eventTypeMeta(item.type);
  return (
    <Card size="small" style={{ borderRadius: 10, border: '1px solid #eae6df' }}>
      <Space size={6} style={{ marginBottom: 6 }} wrap>
        <ProvenanceTag kind={item.kind} />
        <Tag color={tm.color}>{tm.label}</Tag>
      </Space>
      <Paragraph style={{ margin: 0, color: '#33312e' }}>{item.summary}</Paragraph>
    </Card>
  );
};

// ---------- 详情 ----------

const ReportDetailView: React.FC<{ reportId: string }> = ({ reportId }) => {
  const navigate = useNavigate();
  const worldTitles = usePlatformStore((s) => s.worldTitles);
  const [report, setReport] = useState<ReportDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    // GET 详情即打开：服务端回写 opened_at（北极星埋点）。
    cloudFetch<ReportDetail>(`/api/me/reports/${reportId}`)
      .then((d) => {
        if (!cancelled) setReport(d);
      })
      .catch((e) => {
        if (!cancelled) setError(describeCloudError(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [reportId]);

  if (loading) {
    return (
      <div style={{ textAlign: 'center', padding: 80 }}>
        <Spin />
      </div>
    );
  }
  if (error) {
    return (
      <div style={{ padding: '32px 40px', maxWidth: 760, margin: '0 auto' }}>
        <Alert type="error" showIcon message="连接平台失败" description={error} />
        <Button style={{ marginTop: 16 }} icon={<LeftOutlined />} onClick={() => navigate('/platform/reports')}>
          返回日报列表
        </Button>
      </div>
    );
  }
  if (!report) return null;

  const c = report.content;
  const legendEntries = Object.entries(c.provenanceLegend ?? {});

  return (
    <div style={{ padding: '28px 40px', maxWidth: 760, margin: '0 auto' }}>
      <Button type="text" icon={<LeftOutlined />} onClick={() => navigate('/platform/reports')} style={{ marginBottom: 12 }}>
        日报列表
      </Button>

      <Space direction="vertical" size={2} style={{ marginBottom: 16 }}>
        <Title level={2} style={{ margin: 0, color: '#33312e', fontWeight: 500 }}>
          你的角色昨日人生
        </Title>
        <Text type="secondary">
          {c.reportDay} · {worldTitles[report.worldId] || report.worldId} · 角色 {report.characterId}
        </Text>
      </Space>

      {/* 来源分层图例 */}
      {legendEntries.length > 0 && (
        <Card size="small" style={{ marginBottom: 16, background: '#faf9f5', border: '1px solid #eae6df' }}>
          <Space size={16} wrap>
            {legendEntries.map(([kind, label]) => (
              <Space key={kind} size={4}>
                <ProvenanceTag kind={kind} />
                <Text type="secondary" style={{ fontSize: 12 }}>
                  {String(label)}
                </Text>
              </Space>
            ))}
          </Space>
        </Card>
      )}

      {/* 高光事件卡 */}
      <Title level={5} style={{ color: '#8c857b' }}>
        高光
      </Title>
      {c.highlights.length === 0 ? (
        <Empty description="今天是平静的一天" image={Empty.PRESENTED_IMAGE_SIMPLE} />
      ) : (
        <Space direction="vertical" size={10} style={{ width: '100%' }}>
          {c.highlights.map((h, i) => (
            <HighlightCard key={h.eventId || i} item={h} />
          ))}
        </Space>
      )}

      {/* 关系变化 */}
      {c.relationChanges.length > 0 && (
        <>
          <Divider />
          <Title level={5} style={{ color: '#8c857b' }}>
            关系变化
          </Title>
          <Space direction="vertical" size={10} style={{ width: '100%' }}>
            {c.relationChanges.map((r, i) => (
              <HighlightCard key={r.eventId || `rel-${i}`} item={r} />
            ))}
          </Space>
        </>
      )}

      {/* 角色独白 */}
      <Divider />
      <Card
        style={{ borderRadius: 12, border: 'none', background: '#fff7f0' }}
        styles={{ body: { padding: 20 } }}
      >
        <Space size={8} style={{ marginBottom: 8 }}>
          <MessageOutlined style={{ color: '#d97757' }} />
          <Text strong>角色独白</Text>
          <ProvenanceTag kind={c.monologue.kind} />
        </Space>
        <Paragraph style={{ margin: 0, fontStyle: 'italic', color: '#33312e', fontSize: 15 }}>
          “{c.monologue.text}”
        </Paragraph>
      </Card>
    </div>
  );
};

// ---------- 列表 ----------

const ReportListView: React.FC = () => {
  const navigate = useNavigate();
  const { reports, worldTitles, reportsLoading, reportsError, loadReports } = usePlatformStore();

  useEffect(() => {
    void loadReports();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div style={{ padding: '32px 40px', maxWidth: 760, margin: '0 auto' }}>
      <div style={{ marginBottom: 20 }}>
        <Title level={2} style={{ margin: 0, color: '#33312e', fontWeight: 500 }}>
          <ReadOutlined style={{ color: '#d97757', marginRight: 10 }} />
          角色日报
        </Title>
        <Text type="secondary">你的角色每天替你活出的一段人生。</Text>
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
      ) : reportsLoading && reports.length === 0 ? (
        <div style={{ textAlign: 'center', padding: 60 }}>
          <Spin />
        </div>
      ) : reports.length === 0 ? (
        <Empty description="还没有日报，投放角色后每天生成" style={{ padding: 60 }}>
          <Button type="primary" onClick={() => navigate('/platform')}>
            去大厅投放角色
          </Button>
        </Empty>
      ) : (
        <List
          dataSource={reports}
          rowKey={(r) => r.id}
          renderItem={(r) => (
            <Card
              hoverable
              onClick={() => navigate(`/platform/reports/${r.id}`)}
              style={{ marginBottom: 10, borderRadius: 10, border: 'none', boxShadow: '0 1px 3px rgba(0,0,0,0.04)' }}
              styles={{ body: { padding: 16 } }}
            >
              <Space style={{ justifyContent: 'space-between', width: '100%' }}>
                <Space direction="vertical" size={2}>
                  <Space size={8}>
                    <Text strong>{r.reportDay}</Text>
                    {!r.opened && <Badge status="processing" text="未读" />}
                  </Space>
                  <Text type="secondary" style={{ fontSize: 12 }}>
                    {worldTitles[r.worldId] || r.worldId} · 角色 {r.characterId}
                  </Text>
                </Space>
                <Button type="link">阅读</Button>
              </Space>
            </Card>
          )}
        />
      )}
    </div>
  );
};

const DailyReport: React.FC = () => {
  const { id } = useParams<{ id: string }>();
  return id ? <ReportDetailView reportId={id} /> : <ReportListView />;
};

export default DailyReport;
