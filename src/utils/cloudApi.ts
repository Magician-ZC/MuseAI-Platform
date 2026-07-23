// 平台 API 客户端（C0，主循环所有）：与本地 appInvoke 物理分离，云端故障不影响本地能力。
import { useAuthStore } from '../stores/useAuthStore';

const PLATFORM_BASE_KEY = 'museai-platform-base';

export function getPlatformBase(): string {
  if (typeof localStorage !== 'undefined') {
    return localStorage.getItem(PLATFORM_BASE_KEY) || 'http://127.0.0.1:8787';
  }
  return 'http://127.0.0.1:8787';
}

export function setPlatformBase(url: string): void {
  if (typeof localStorage !== 'undefined') {
    localStorage.setItem(PLATFORM_BASE_KEY, url);
  }
}

export class CloudError extends Error {
  constructor(public code: string, message: string, public status: number) {
    super(message);
  }
}

function newIdempotencyKey(): string {
  if (typeof crypto !== 'undefined' && crypto.randomUUID) return crypto.randomUUID();
  return `idem-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

interface CloudFetchOptions {
  method?: string;
  body?: unknown;
  // 副作用请求传 true：自动附带 Idempotency-Key，失败重试不重复副作用
  idempotent?: boolean;
  // 401 时是否尝试 refresh 后重试一次（默认 true）
  retryOnAuth?: boolean;
}

/**
 * 平台 API 调用：自动附带 access token、失败刷新、稳定错误码抛 CloudError。
 * 与 appInvoke（本地 Tauri 命令）完全独立。
 */
export async function cloudFetch<T>(path: string, options: CloudFetchOptions = {}): Promise<T> {
  const { method = 'GET', body, idempotent = false, retryOnAuth = true } = options;
  const auth = useAuthStore.getState();
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (auth.accessToken) headers['Authorization'] = `Bearer ${auth.accessToken}`;
  if (idempotent) headers['Idempotency-Key'] = newIdempotencyKey();

  const res = await fetch(`${getPlatformBase()}${path}`, {
    method,
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });

  if (res.status === 401 && retryOnAuth && auth.refreshToken) {
    const refreshed = await auth.refresh();
    if (refreshed) {
      return cloudFetch<T>(path, { ...options, retryOnAuth: false });
    }
    auth.logout();
    throw new CloudError('unauthorized', '登录已过期，请重新登录', 401);
  }

  const text = await res.text();
  const data = text ? JSON.parse(text) : undefined;
  if (!res.ok) {
    const err = data?.error ?? { code: 'unknown', message: `HTTP ${res.status}` };
    throw new CloudError(err.code, err.message, res.status);
  }
  return data as T;
}

/** 订阅世界事件流（WS）；返回退订函数。断线自动重连并按 lastEventId 补偿。 */
export function cloudStream(
  worldId: string,
  onEvent: (event: unknown) => void,
  onError?: (err: unknown) => void
): () => void {
  const auth = useAuthStore.getState();
  let closed = false;
  let ws: WebSocket | null = null;
  let lastEventId = '';
  let retryTimer: ReturnType<typeof setTimeout> | null = null;

  const connect = () => {
    if (closed) return;
    const base = getPlatformBase().replace(/^http/, 'ws');
    const params = new URLSearchParams({ token: auth.accessToken || '' });
    if (lastEventId) params.set('lastEventId', lastEventId);
    ws = new WebSocket(`${base}/api/worlds/${worldId}/stream?${params.toString()}`);
    ws.onmessage = (e) => {
      try {
        const payload = JSON.parse(e.data);
        if (payload?.id) lastEventId = payload.id;
        onEvent(payload);
      } catch (err) {
        onError?.(err);
      }
    };
    ws.onerror = (err) => onError?.(err);
    ws.onclose = () => {
      if (!closed) retryTimer = setTimeout(connect, 2000);
    };
  };
  connect();

  return () => {
    closed = true;
    if (retryTimer) clearTimeout(retryTimer);
    ws?.close();
  };
}
