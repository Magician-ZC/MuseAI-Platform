// 后台 API 客户端（A0，主循环所有）。管理员 token 存 sessionStorage（后台会话不持久化到磁盘）。
const BASE = (import.meta as any).env?.VITE_ADMIN_API || 'http://127.0.0.1:8787';

const TOKEN_KEY = 'museai-admin-token';
const ROLE_KEY = 'museai-admin-role';

export function getToken(): string | null {
  return sessionStorage.getItem(TOKEN_KEY);
}
export function setToken(t: string | null): void {
  if (t) sessionStorage.setItem(TOKEN_KEY, t);
  else sessionStorage.removeItem(TOKEN_KEY);
}

// #9 RBAC：保存 dev-login 返回的 role，供前端收敛可见模块（纵深防御，后端仍权威）。
export function getRole(): string | null {
  return sessionStorage.getItem(ROLE_KEY);
}
export function setRole(r: string | null): void {
  if (r) sessionStorage.setItem(ROLE_KEY, r);
  else sessionStorage.removeItem(ROLE_KEY);
}

/** 退出登录：清除 token 与 role（后台会话整体失效）。 */
export function clearSession(): void {
  setToken(null);
  setRole(null);
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
