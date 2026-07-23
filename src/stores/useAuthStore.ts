// 平台登录态（C0，主循环所有）。token 存 localStorage（桌面端也可迁移到 OS keychain，接口预留）。
import { create } from 'zustand';
import { persist, createJSONStorage } from 'zustand/middleware';

export interface PlatformUser {
  id: string;
  nickname: string;
  phone?: string;
  ageDeclared: number;
}

interface AuthState {
  accessToken: string | null;
  refreshToken: string | null;
  user: PlatformUser | null;
  isAuthed: () => boolean;
  setSession: (accessToken: string, refreshToken: string, user: PlatformUser) => void;
  logout: () => void;
  /** 用 refreshToken 换新 access；成功返回 true。实现见 C1（登录流程）；此处提供占位以打通类型。 */
  refresh: () => Promise<boolean>;
}

export const useAuthStore = create<AuthState>()(
  persist(
    (set, get) => ({
      accessToken: null,
      refreshToken: null,
      user: null,
      isAuthed: () => !!get().accessToken,
      setSession: (accessToken, refreshToken, user) => set({ accessToken, refreshToken, user }),
      logout: () => set({ accessToken: null, refreshToken: null, user: null }),
      refresh: async () => {
        const rt = get().refreshToken;
        if (!rt) return false;
        try {
          const base =
            (typeof localStorage !== 'undefined' && localStorage.getItem('museai-platform-base')) ||
            'http://127.0.0.1:8787';
          const res = await fetch(`${base}/api/auth/refresh`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ refreshToken: rt }),
          });
          if (!res.ok) return false;
          const data = await res.json();
          set({ accessToken: data.accessToken, refreshToken: data.refreshToken });
          return true;
        } catch {
          return false;
        }
      },
    }),
    {
      name: 'museai-auth',
      storage: createJSONStorage(() => localStorage),
      version: 1,
    }
  )
);
