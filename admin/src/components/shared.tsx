// A1 共享设施：友好错误、时间格式化、cursor 分页 hook、理由输入 Modal、错误卡。
// 仅本后台页面复用；不改动 api.ts / App.tsx 契约。
import { useCallback, useEffect, useRef, useState } from 'react';
import { Alert, Button, Input, Modal } from 'antd';
import { AdminApiError } from '../api';

// 稳定 error code → 简体中文文案（对齐 server/src/error.rs）。
const CODE_TEXT: Record<string, string> = {
  unauthorized: '登录已过期或未认证，请重新登录',
  forbidden: '无权限执行此操作',
  not_found: '目标不存在或已被移除',
  bad_request: '请求参数有误',
  conflict: '状态冲突：目标可能已被处理',
  idempotency_mismatch: '幂等键重复但载荷不一致',
  risk_blocked: '已被风控拦截',
  internal: '服务端内部错误，请稍后重试',
};

/** 任意异常 → 友好中文提示（区分 API 错误、网络错误、其它）。 */
export function friendlyError(e: unknown): string {
  if (e instanceof AdminApiError) {
    return CODE_TEXT[e.code] ?? e.message ?? '操作失败';
  }
  // fetch 网络层失败抛 TypeError（后端未启动 / 跨域 / 断网）。
  if (e instanceof TypeError) {
    return '无法连接后台服务，请确认 server 已启动（默认 127.0.0.1:8787）';
  }
  return e instanceof Error ? e.message : '操作失败';
}

/** epoch 毫秒 → 本地时间字符串；空值显示占位符。 */
export function formatTime(ms?: number | null): string {
  if (ms == null) return '—';
  const d = new Date(ms);
  return Number.isNaN(d.getTime()) ? '—' : d.toLocaleString('zh-CN', { hour12: false });
}

/** 千分位数字。 */
export function formatNumber(n?: number | null): string {
  return n == null ? '—' : n.toLocaleString('zh-CN');
}

/** 比率 → 百分比字符串。 */
export function formatPercent(r?: number | null, digits = 1): string {
  return r == null ? '—' : `${(r * 100).toFixed(digits)}%`;
}

interface PagedResult<T> {
  items: T[];
  nextCursor: string | null;
}

/**
 * cursor 分页 hook（"加载更多"模式）。
 * fetcher 每次接收当前 cursor（首屏为 null），返回统一 { items, nextCursor }。
 * 页面在筛选变化时调 reload()；追加下一页调 loadMore()。
 * fetcher 用 ref 持有最新闭包，保证 reload/loadMore 始终读到最新筛选值。
 */
export function usePagedList<T>(fetcher: (cursor: string | null) => Promise<PagedResult<T>>) {
  const [items, setItems] = useState<T[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const fetcherRef = useRef(fetcher);
  fetcherRef.current = fetcher;

  const run = useCallback(async (cursor: string | null, append: boolean) => {
    setLoading(true);
    setError(null);
    try {
      const res = await fetcherRef.current(cursor);
      setItems((prev) => (append ? [...prev, ...res.items] : res.items));
      setNextCursor(res.nextCursor ?? null);
    } catch (e) {
      setError(friendlyError(e));
      if (!append) setItems([]);
    } finally {
      setLoading(false);
    }
  }, []);

  const reload = useCallback(() => run(null, false), [run]);
  const loadMore = useCallback(() => run(nextCursor, true), [run, nextCursor]);

  return { items, loading, error, hasMore: nextCursor != null, reload, loadMore };
}

/** 页面级加载失败提示（优雅降级，不崩溃）。 */
export function ErrorAlert({ message, onRetry }: { message: string; onRetry?: () => void }) {
  return (
    <Alert
      type="error"
      showIcon
      message="加载失败"
      description={message}
      style={{ marginBottom: 16 }}
      action={
        onRetry ? (
          <Button size="small" danger onClick={onRetry}>
            重试
          </Button>
        ) : undefined
      }
    />
  );
}

/**
 * 理由输入 Modal：动作端点（ban/pause/activate…）的理由走 query ?reason=，写入审计日志。
 * open 为受控；每次打开清空输入。
 */
export function ReasonModal({
  open,
  title,
  placeholder,
  okText,
  danger,
  loading,
  onOk,
  onCancel,
}: {
  open: boolean;
  title: string;
  placeholder?: string;
  okText?: string;
  danger?: boolean;
  loading?: boolean;
  onOk: (reason: string) => void;
  onCancel: () => void;
}) {
  const [reason, setReason] = useState('');
  useEffect(() => {
    if (open) setReason('');
  }, [open]);
  return (
    <Modal
      open={open}
      title={title}
      onOk={() => onOk(reason.trim())}
      onCancel={onCancel}
      confirmLoading={loading}
      okText={okText ?? '确定'}
      cancelText="取消"
      okButtonProps={{ danger }}
    >
      <Input.TextArea
        rows={3}
        value={reason}
        placeholder={placeholder ?? '填写操作理由（可选，将写入审计日志）'}
        onChange={(e) => setReason(e.target.value)}
      />
    </Modal>
  );
}
