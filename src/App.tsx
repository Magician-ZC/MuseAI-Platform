import { BrowserRouter as Router, Routes, Route, Navigate } from 'react-router-dom';
import AppShell from './components/AppShell';
import Home from './pages/Home';
import Works from './pages/Works';
import Settings from './pages/Settings';
import DeAi from './pages/DeAi';
import Outline from './pages/Outline';
import Examples from './pages/Examples';
import Background from './pages/Background';
import Chat from './pages/Chat';
import Story from './pages/Story';
import Adventure from './pages/Adventure';
import Bond from './pages/Bond';
import BookTravelMaterials from './pages/BookTravelMaterials';

// Platform mode (登录后接入云端平台世界；独立路由组，不影响本地页面)
import PlatformShell, { RequireAuth } from './pages/platform/PlatformShell';
import PlatformLogin from './pages/platform/PlatformLogin';
import PlatformHall from './pages/platform/PlatformHall';
import CharacterPublish from './pages/platform/CharacterPublish';
import MyWorlds from './pages/platform/MyWorlds';
import WorldRoom from './pages/platform/WorldRoom';
import WorldSpectate from './pages/platform/WorldSpectate';
import DailyReport from './pages/platform/DailyReport';
import Wallet from './pages/platform/Wallet';
import ArenaHost from './pages/platform/ArenaHost';
import ArenaSpectate from './pages/platform/ArenaSpectate';

// Mobile components
import MobileShell from './components/MobileShell';
import MobileHome from './pages/MobileHome';
import MobileChat from './pages/MobileChat';
import MobileStory from './pages/MobileStory';
import MobileBond from './pages/MobileBond';

import { useSettingsStore } from './stores/useSettingsStore';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useEffect } from 'react';
import { isMobile } from './utils/runtime';
import { applyPartnerStoreContent } from './utils/partnerStoreSync';
import './App.css';

function App() {
  const setWorksDirectory = useSettingsStore((s) => s.setWorksDirectory);
  const mobileEnv = isMobile();

  useEffect(() => {
    // Only invoke desktop setup commands on desktop
    if (!mobileEnv) {
      invoke<string>('get_workspace_dir', { dirType: 'articles' })
        .then((dir) => setWorksDirectory(dir))
        .catch((err) => console.error('Failed to initialize workspace directory:', err));
    }
  }, [mobileEnv, setWorksDirectory]);

  useEffect(() => {
    if (mobileEnv) return;

    let unlistenFn: (() => void) | undefined;
    listen('partner-store-updated', async () => {
      try {
        const content = await invoke<string>('load_app_state', { name: 'partner-store' });
        applyPartnerStoreContent(content);
      } catch (err) {
        console.error('Failed to sync partner store:', err);
      }
    }).then((fn) => {
      unlistenFn = fn;
    });

    return () => {
      if (unlistenFn) unlistenFn();
    };
  }, [mobileEnv]);

  return (
    <Router>
      <Routes>
        {mobileEnv ? (
          <Route path="/" element={<MobileShell />}>
            <Route index element={<MobileHome />} />
            <Route path="chat" element={<MobileChat />} />
            <Route path="story" element={<MobileStory />} />
            <Route path="bond" element={<MobileBond />} />
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        ) : (
          <>
            <Route path="/" element={<AppShell />}>
              <Route index element={<Home />} />
              <Route path="works" element={<Works />} />
              <Route path="settings" element={<Settings />} />
              <Route path="examples" element={<Examples />} />
              <Route path="de-ai" element={<DeAi />} />
              <Route path="outline" element={<Outline />} />
              <Route path="background" element={<Background />} />
              <Route path="chat" element={<Chat />} />
              <Route path="adventure" element={<Adventure />} />
              <Route path="story" element={<Story />} />
              <Route path="bond" element={<Bond />} />
              <Route path="book-travel-materials" element={<BookTravelMaterials />} />
            </Route>
            {/* 平台模式路由组：独立外壳，未登录访问受保护页 → 引导登录；本地页面完全不受影响 */}
            <Route path="/platform" element={<PlatformShell />}>
              <Route index element={<RequireAuth><PlatformHall /></RequireAuth>} />
              <Route path="login" element={<PlatformLogin />} />
              <Route path="publish" element={<RequireAuth><CharacterPublish /></RequireAuth>} />
              <Route path="my" element={<RequireAuth><MyWorlds /></RequireAuth>} />
              <Route path="worlds/:id" element={<RequireAuth><WorldRoom /></RequireAuth>} />
              <Route path="worlds/:id/spectate" element={<RequireAuth><WorldSpectate /></RequireAuth>} />
              <Route path="reports" element={<RequireAuth><DailyReport /></RequireAuth>} />
              <Route path="reports/:id" element={<RequireAuth><DailyReport /></RequireAuth>} />
              {/* P4b 钱包 + P6 赛事房（独立平台页；本地页面不受影响） */}
              <Route path="wallet" element={<RequireAuth><Wallet /></RequireAuth>} />
              <Route path="arena/:worldId/host" element={<RequireAuth><ArenaHost /></RequireAuth>} />
              <Route path="arena/:worldId/spectate" element={<RequireAuth><ArenaSpectate /></RequireAuth>} />
            </Route>
          </>
        )}
      </Routes>
    </Router>
  );
}

export default App;
