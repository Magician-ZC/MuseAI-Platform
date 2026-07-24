// 跨世界背包（C1，规格 §2.5 章节房 + §9.6 服务端权威）：只读 /me/backpack，按状态分组呈现。
// 物品取得只有服务端两条合法写入路径（通关结算 / 支付履约），此页纯展示，无任何"声明拥有"入口。
import React, { useEffect } from 'react';
import { Typography, Card, Tag, Alert, Spin, Empty, Space, Divider, Tooltip } from 'antd';
import { ShoppingOutlined, GlobalOutlined, ThunderboltOutlined } from '@ant-design/icons';
import { usePlatformStore, type BackpackItem } from '../../stores/usePlatformStore';

const { Title, Text, Paragraph } = Typography;

/** 背包物品状态 → 展示元数据（owned/carried/sealed/consumed；后端默认已排除 consumed）。 */
const STATUS_META: Record<string, { label: string; color: string; hint: string }> = {
  carried: { label: '随身', color: 'green', hint: '已随角色入场携带，在目标世界生效' },
  owned: { label: '在库', color: 'default', hint: '存放在账号背包，尚未随角色入场' },
  sealed: { label: '封印', color: 'volcano', hint: '入场被目标世界降档封存，暂不可用' },
  consumed: { label: '已消耗', color: 'default', hint: '已在剧情中被使用' },
};

/** 分组展示顺序（随身在前，最贴近"当前正在世界里用"）。 */
const GROUP_ORDER = ['carried', 'owned', 'sealed', 'consumed'];

const ItemCard: React.FC<{ item: BackpackItem }> = ({ item }) => {
  const worldTitles = usePlatformStore((s) => s.worldTitles);
  const meta = STATUS_META[item.status] ?? { label: item.status, color: 'default', hint: '' };
  const acquiredTitle = worldTitles[item.acquiredWorldId] || item.acquiredWorldId;
  const carriedTitle = item.carriedWorldId ? worldTitles[item.carriedWorldId] || item.carriedWorldId : null;
  return (
    <Card size="small" style={{ borderRadius: 10, border: '1px solid #eae6df' }} styles={{ body: { padding: 16 } }}>
      <Space style={{ justifyContent: 'space-between', width: '100%' }} align="start">
        <Space size={8} wrap>
          <Text strong style={{ color: '#33312e' }}>
            {item.item.id}
          </Text>
          <Tooltip title={meta.hint}>
            <Tag color={meta.color}>{meta.label}</Tag>
          </Tooltip>
          <Tag color="gold">
            <ThunderboltOutlined /> 强度 {item.item.origin.powerTier}
          </Tag>
        </Space>
      </Space>
      <Paragraph style={{ margin: '8px 0 4px', color: '#33312e' }}>{item.item.narrative || '（无叙事描述）'}</Paragraph>
      {item.item.effectTags.length > 0 && (
        <Space size={4} wrap style={{ marginBottom: 4 }}>
          {item.item.effectTags.map((t) => (
            <Tag key={t} color="geekblue">
              {t}
            </Tag>
          ))}
        </Space>
      )}
      <Space size={16} style={{ color: '#8c857b', fontSize: 12 }} wrap>
        <span>
          <GlobalOutlined /> 得自：{acquiredTitle}
        </span>
        {carriedTitle && <span>携带于：{carriedTitle}</span>}
        {item.item.origin.cosmology.length > 0 && <span>体系：{item.item.origin.cosmology.join(' / ')}</span>}
      </Space>
    </Card>
  );
};

const Backpack: React.FC = () => {
  const { backpack, backpackLoading, backpackError, loadBackpack } = usePlatformStore();

  useEffect(() => {
    void loadBackpack();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 按状态分组（保持后端返回的 acquiredAt DESC 组内序）。
  const groups = GROUP_ORDER.map((status) => ({
    status,
    meta: STATUS_META[status],
    items: backpack.filter((b) => b.status === status),
  })).filter((g) => g.items.length > 0);

  return (
    <div style={{ padding: '32px 40px', maxWidth: 900, margin: '0 auto' }}>
      <div style={{ marginBottom: 16 }}>
        <Title level={2} style={{ margin: 0, color: '#33312e', fontWeight: 500 }}>
          <ShoppingOutlined style={{ color: '#d97757', marginRight: 10 }} />
          跨世界背包
        </Title>
        <Text type="secondary">角色通关所得的信物，可随其他角色入场携带（经准入判定生效或降档封印）。</Text>
      </div>

      <Alert
        type="info"
        showIcon
        style={{ marginBottom: 20 }}
        message="携带经入场生效，主动投放后续开放"
        description="物品只由服务端在通关结算 / 支付履约时入包（无客户端声明入口）。跨世界携带在入场（carry）时按目标世界准入判定；在世界内主动投放道具的干预将于后续开放。"
      />

      {backpackError ? (
        <Alert type="error" showIcon message="连接平台失败" description={backpackError} />
      ) : backpackLoading && backpack.length === 0 ? (
        <div style={{ textAlign: 'center', padding: 60 }}>
          <Spin />
        </div>
      ) : backpack.length === 0 ? (
        <Empty description="背包还是空的——让角色在世界里通关，赢取第一件跨世界信物" style={{ padding: 60 }} />
      ) : (
        <Space direction="vertical" size={20} style={{ width: '100%' }}>
          {groups.map((g) => (
            <div key={g.status}>
              <Divider titlePlacement="start" style={{ margin: '0 0 12px' }}>
                <Space size={8}>
                  <Tag color={g.meta.color}>{g.meta.label}</Tag>
                  <Text type="secondary" style={{ fontSize: 13 }}>
                    {g.items.length} 件
                  </Text>
                </Space>
              </Divider>
              <Space direction="vertical" size={12} style={{ width: '100%' }}>
                {g.items.map((item) => (
                  <ItemCard key={item.backpackId} item={item} />
                ))}
              </Space>
            </div>
          ))}
        </Space>
      )}
    </div>
  );
};

export default Backpack;
