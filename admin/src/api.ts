// 后台 API 客户端（A0，主循环所有）。管理员 token 存 sessionStorage（后台会话不持久化到磁盘）。
const BASE = (import.meta as any).env?.VITE_ADMIN_API || 'http://127.0.0.1:8787';

export function getToken(): string | null {
  return sessionStorage.getItem('museai-admin-token');
}
export function setToken(t: string | null): void {
  if (t) sessionStorage.setItem('museai-admin-token', t);
  else sessionStorage.removeItem('museai-admin-token');
}

export class AdminApiError extends Error {
  constructor(public code: string, message: string) {
    super(message);
  }
}

export async function adminFetch<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  const token = getToken();
  if (token) headers['Authorization'] = `Bearer ${token}`;
  const res = await fetch(`${BASE}/api${path}`, {
    method,
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  const data = text ? JSON.parse(text) : undefined;
  if (!res.ok) {
    const err = data?.error ?? { code: 'unknown', message: `HTTP ${res.status}` };
    throw new AdminApiError(err.code, err.message);
  }
  return data as T;
}
