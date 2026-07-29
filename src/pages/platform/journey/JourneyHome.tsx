import React from 'react';
import {
  ApartmentOutlined,
  AuditOutlined,
  BulbOutlined,
  CompassOutlined,
  GiftOutlined,
  LinkOutlined,
  PlayCircleOutlined,
  ReadOutlined,
  TeamOutlined,
} from '@ant-design/icons';
import { Button, Tag } from 'antd';
import { Link, useNavigate } from 'react-router-dom';
import { journeyHref, useJourneyPreview } from './JourneyShared';
import './Journey.css';

const features = [
  { path: 'onboarding', icon: <GiftOutlined />, title: '从第一段旅程开始', text: '领取开场礼，选择角色预设并开启微世界。' },
  { path: 'invitations', icon: <TeamOutlined />, title: '房间邀请', text: '查看、接受或婉拒来自角色面具的邀请。' },
  { path: 'ooc', icon: <AuditOutlined />, title: '角色解释权', text: '对落定节拍提出 OOC 申诉，保存私人批注。' },
  { path: 'iflines', icon: <ApartmentOutlined />, title: '开启私人平行线', text: '从可信分叉点出发，体验不改写原世界的另一种可能。' },
  { path: 'subplot', icon: <BulbOutlined />, title: '我的副本卡', text: '整理旅程碎片，用同星卡合成新的剧情蓝图。' },
  { path: 'social', icon: <LinkOutlined />, title: '解锁真实身份', text: '在双方同意后，把角色关系延伸到现实社交。' },
  { path: 'chapters', icon: <ReadOutlined />, title: '欢迎回到故事里', text: '查看离线成长与跨世界物品，继续章节房。' },
  { path: 'live', icon: <PlayCircleOutlined />, title: '今夜开演', text: '跟随公开舞台事件流，在不泄露身份的前提下互动。' },
];

const JourneyHome: React.FC = () => {
  const preview = useJourneyPreview();
  const navigate = useNavigate();
  return (
    <main className="journey-page journey-page--wide">
      <header className="journey-page__header">
        <span className="journey-eyebrow">MUSEAI JOURNEY</span>
        <div className="journey-page__heading">
          <div>
            <h1>我的旅程</h1>
            <p>从开场礼到封卷、从平行故事到公开舞台，所有长期叙事资产都在这里延续。</p>
          </div>
          {preview && <Tag color="orange">设计预览</Tag>}
        </div>
      </header>

      <section className="journey-panel journey-panel--hero" aria-labelledby="journey-current-title">
        <div className="journey-hero">
          <img className="journey-hero__cover" src="/assets/platform/mist-sea-world.png" alt="雾海城堡与海岸" />
          <div className="journey-hero__shade" />
          <div className="journey-hero__copy">
            <Tag color="volcano" style={{ width: 'fit-content', margin: 0 }}>当前世界 · 进行中</Tag>
            <h2 id="journey-current-title">雾海纪元</h2>
            <p>凯恩·夜誓刚刚越过旧港的雾墙。下一拍，他将决定是回应灯塔的求援，还是追踪潮汐里那个被抹去的名字。</p>
            <div style={{ marginTop: 24 }}>
              <Button type="primary" size="large" icon={<CompassOutlined />} onClick={() => navigate(journeyHref('/platform/journey/live', preview))}>
                继续这段旅程
              </Button>
            </div>
          </div>
          <div className="journey-hero__side">
            <img src="/assets/characters/kane-night-oath-portrait.png" alt="凯恩·夜誓角色肖像" />
          </div>
        </div>
      </section>

      <section className="journey-home-grid" aria-label="旅程功能">
        {features.map((feature) => (
          <Link className="journey-home-card" key={feature.path} to={journeyHref(`/platform/journey/${feature.path}`, preview)}>
            <span className="journey-home-card__icon" aria-hidden="true">{feature.icon}</span>
            <h3>{feature.title}</h3>
            <p>{feature.text}</p>
            <span>进入功能 →</span>
          </Link>
        ))}
      </section>
    </main>
  );
};

export default JourneyHome;
