import React, { useEffect, useMemo, useState } from 'react';
import { Alert, Button, Empty, Select, Spin, Tag } from 'antd';
import { ArrowLeftOutlined, CheckCircleFilled, CompassOutlined } from '@ant-design/icons';
import { useLocation, useNavigate, useSearchParams } from 'react-router-dom';
import {
  journeyError,
  loadJourneyContext,
  previewContext,
  previewMemberships,
  type JourneyContext,
  type JourneyMembership,
} from './journeyData';
import './Journey.css';

export function useJourneyPreview(): boolean {
  const location = useLocation();
  return import.meta.env.DEV && new URLSearchParams(location.search).get('design') === 'preview';
}

export function journeyHref(path: string, preview: boolean): string {
  return `${path}${preview ? '?design=preview' : ''}`;
}

export const JourneyPage: React.FC<{
  eyebrow?: string;
  title: string;
  description: string;
  children: React.ReactNode;
  action?: React.ReactNode;
  wide?: boolean;
}> = ({ eyebrow = '我的旅程', title, description, children, action, wide }) => {
  const navigate = useNavigate();
  const preview = useJourneyPreview();
  return (
    <main className={`journey-page${wide ? ' journey-page--wide' : ''}`}>
      <header className="journey-page__header">
        <button type="button" className="journey-back" onClick={() => navigate(journeyHref('/platform/journey', preview))}>
          <ArrowLeftOutlined /> 返回旅程
        </button>
        <div className="journey-page__heading">
          <div>
            <span className="journey-eyebrow">{eyebrow}</span>
            <h1>{title}</h1>
            <p>{description}</p>
          </div>
          {action}
        </div>
      </header>
      {preview && (
        <Alert
          className="journey-preview-note"
          type="info"
          showIcon
          title="设计预览数据"
          description="此模式仅用于视觉与交互验收；正式访问会读取平台真实数据并执行真实接口。"
        />
      )}
      {children}
    </main>
  );
};

export const JourneyState: React.FC<{
  loading?: boolean;
  error?: string | null;
  empty?: boolean;
  emptyText?: string;
  onRetry?: () => void;
  children: React.ReactNode;
}> = ({ loading, error, empty, emptyText = '暂时没有可展示的内容', onRetry, children }) => {
  if (loading) return <div className="journey-state"><Spin size="large" /></div>;
  if (error) return <Alert type="error" showIcon title="暂时无法读取" description={error} action={onRetry ? <Button onClick={onRetry}>重试</Button> : undefined} />;
  if (empty) return <div className="journey-state"><Empty description={emptyText} /></div>;
  return <>{children}</>;
};

export const JourneyContextBar: React.FC<{
  context: JourneyContext | null;
  memberships: JourneyMembership[];
  onChange?: (worldId: string) => void;
}> = ({ context, memberships, onChange }) => (
  <div className="journey-context-bar">
    <span><CompassOutlined /> 当前故事</span>
    <Select
      aria-label="选择当前故事世界"
      value={context?.worldId}
      placeholder="选择一个世界"
      options={memberships.map((item) => ({ value: item.worldId, label: `${item.worldTitle} · ${item.characterName}` }))}
      onChange={onChange}
      disabled={!onChange || memberships.length < 2}
      popupMatchSelectWidth={false}
    />
    {context && <Tag icon={<CheckCircleFilled />} color="success">{context.characterName}</Tag>}
  </div>
);

export function useJourneyContext(): {
  context: JourneyContext | null;
  memberships: JourneyMembership[];
  loading: boolean;
  error: string | null;
  changeWorld: (worldId: string) => void;
  reload: () => void;
} {
  const preview = useJourneyPreview();
  const [searchParams, setSearchParams] = useSearchParams();
  const requestedWorld = searchParams.get('worldId') || undefined;
  const [context, setContext] = useState<JourneyContext | null>(preview ? previewContext : null);
  const [memberships, setMemberships] = useState<JourneyMembership[]>(preview ? previewMemberships : []);
  const [loading, setLoading] = useState(!preview);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    if (preview) {
      setContext(previewContext);
      setMemberships(previewMemberships);
      setLoading(false);
      return;
    }
    let live = true;
    setLoading(true);
    setError(null);
    loadJourneyContext(requestedWorld)
      .then((result) => {
        if (!live) return;
        setContext(result.context);
        setMemberships(result.memberships);
      })
      .catch((err) => live && setError(journeyError(err)))
      .finally(() => live && setLoading(false));
    return () => { live = false; };
  }, [preview, requestedWorld, revision]);

  return useMemo(() => ({
    context,
    memberships,
    loading,
    error,
    changeWorld: (worldId: string) => {
      const next = new URLSearchParams(searchParams);
      next.set('worldId', worldId);
      setSearchParams(next);
    },
    reload: () => setRevision((value) => value + 1),
  }), [context, memberships, loading, error, searchParams, setSearchParams]);
}

export const StarRating: React.FC<{ value: number }> = ({ value }) => (
  <span className="journey-stars" aria-label={`${value} 星`}>
    {'★'.repeat(Math.max(0, value))}<span>{'★'.repeat(Math.max(0, 5 - value))}</span>
  </span>
);
