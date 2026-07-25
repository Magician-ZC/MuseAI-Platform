// 后台 API 客户端（A0，主循环所有）。管理员 token 存 sessionStorage（后台会话不持久化到磁盘）。
const BASE = (import.meta as any).env?.VITE_ADMIN_API || 'http://127.0.0.1:8787';

// ---------------- 环境标识 ----------------
// 设计文档 §8：生产环境必须展示真实环境标识，避免运营人员在错误环境执行操作。

export type AdminEnvKey = 'production' | 'staging' | 'development' | 'unknown';

export interface AdminEnvironment {
  key: AdminEnvKey;
  label: string;
  /** 后台实际连接的接口基址（判定依据之一，同时展示给运营核对）。 */
  apiBase: string;
  /** Vite 构建模式，仅作辅助信息，不单独作为环境结论。 */
  buildMode: string;
}

const ENV_LABEL: Record<AdminEnvKey, string> = {
  production: '生产环境',
  staging: '预发环境',
  development: '开发环境',
  unknown: '环境未知',
};

const ENV_ALIAS: Record<string, AdminEnvKey> = {
  prod: 'production',
  production: 'production',
  staging: 'staging',
  stage: 'staging',
  pre: 'staging',
  dev: 'development',
  development: 'development',
  local: 'development',
};

const LOOPBACK_BASE = /^https?:\/\/(127\.0\.0\.1|localhost|0\.0\.0\.0|\[::1\])(:\d+)?(\/|$)/i;

/**
 * 判定当前后台连接的环境。
 * 顺序：显式注入的 VITE_ADMIN_ENV → Vite dev 模式 → 接口基址为本地回环 → 「环境未知」。
 * 注意不能用 import.meta.env.MODE==='production' 直接判生产：同一份构建产物可以部署到任意环境，
 * 判不出来时宁可显示「环境未知」，也不能假装生产环境。
 */
export function resolveEnvironment(): AdminEnvironment {
  const env = (import.meta as any).env ?? {};
  const buildMode = String(env.MODE ?? 'unknown');
  const declared = String(env.VITE_ADMIN_ENV ?? '').trim().toLowerCase();
  const mapped = ENV_ALIAS[declared];
  if (mapped) return { key: mapped, label: ENV_LABEL[mapped], apiBase: BASE, buildMode };
  if (env.DEV) return { key: 'development', label: ENV_LABEL.development, apiBase: BASE, buildMode };
  if (LOOPBACK_BASE.test(BASE)) return { key: 'development', label: ENV_LABEL.development, apiBase: BASE, buildMode };
  return { key: 'unknown', label: ENV_LABEL.unknown, apiBase: BASE, buildMode };
}

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
