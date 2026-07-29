// 路由级代码分割（2026-07-29）：43 条路由此前全部静态 import，于是**每一个宿主的首屏**
// 都要先把整包啃完——实测入口 chunk 3,633,354 B（gzip 1,145,655），其中 echarts + zrender
// 一家占源码字节的 40%，而全仓 41 个路由页里只有 6 个真的用得上它。
//
// 🔴 真正的收益在**手机端**，而且比 gzip 数字更硬：`mobile_server.rs` 里没有任何压缩层
//    （全仓 Compression/gzip/deflate 零命中），手机浏览器下载的是原始字节。
//    改后手机端首屏闭包 3,633,354 B → 853,273 B，走的还是 Wi-Fi。
// 🔵 桌面 Tauri 是本地加载（`frontendDist`，随包走自定义协议），买到的不是下载时间而是
//    V8 的解析/编译时间：现在启动要先啃完整包才有首屏，改完约 2.8MB 推迟到用户真去那一页才付。
//    两个数不是一回事，别混着说。
//
// ⚠️ 代价如实说：切页时内容区会出现一次 loading。Suspense 刻意放在**三个壳内部、包住 Outlet**
//    （不是包在 Routes 外面）——那样壳不闪，只有内容区转圈。fallback 沿用仓库里已有的那一个
//    （`MarkdownEditor.tsx` 的 Spin 包装），不新造设计——抽成 `components/RouteFallback.tsx`。
import { lazy, useEffect } from 'react';
import { BrowserRouter as Router, Routes, Route, Navigate } from 'react-router-dom';
import AppShell from './components/AppShell';

const Home = lazy(() => import('./pages/Home'));
const Works = lazy(() => import('./pages/Works'));
const Settings = lazy(() => import('./pages/Settings'));
const DeAi = lazy(() => import('./pages/DeAi'));
const Outline = lazy(() => import('./pages/Outline'));
const Examples = lazy(() => import('./pages/Examples'));
const Background = lazy(() => import('./pages/Background'));
const Chat = lazy(() => import('./pages/Chat'));
const Story = lazy(() => import('./pages/Story'));
const Adventure = lazy(() => import('./pages/Adventure'));
const Bond = lazy(() => import('./pages/Bond'));
const BookTravelMaterials = lazy(() => import('./pages/BookTravelMaterials'));

// Platform mode（登录后接入云端平台世界；独立路由组，不影响本地页面）
// 🔴 PlatformShell 与 RequireAuth 必须保持静态：它们是**壳与鉴权守卫**，
//    懒加载它们等于把「要不要登录」这个判定也推迟到下载完成之后。
import PlatformShell, { RequireAuth } from './pages/platform/PlatformShell';
const PlatformLogin = lazy(() => import('./pages/platform/PlatformLogin'));
const PlatformHall = lazy(() => import('./pages/platform/PlatformHall'));
const CharacterPublish = lazy(() => import('./pages/platform/CharacterPublish'));
const WorldPublish = lazy(() => import('./pages/platform/WorldPublish'));
const MyWorlds = lazy(() => import('./pages/platform/MyWorlds'));
const MyCharacters = lazy(() => import('./pages/platform/MyCharacters'));
const CharacterArchive = lazy(() => import('./pages/platform/CharacterArchive'));
const Backpack = lazy(() => import('./pages/platform/Backpack'));
const Bonds = lazy(() => import('./pages/platform/Bonds'));
const WorldRoom = lazy(() => import('./pages/platform/WorldRoom'));
const WorldSpectate = lazy(() => import('./pages/platform/WorldSpectate'));
const DailyReport = lazy(() => import('./pages/platform/DailyReport'));
const Wallet = lazy(() => import('./pages/platform/Wallet'));
const ArenaHost = lazy(() => import('./pages/platform/ArenaHost'));
const ArenaSpectate = lazy(() => import('./pages/platform/ArenaSpectate'));
const ArenaReplay = lazy(() => import('./pages/platform/ArenaReplay'));
const JourneyHome = lazy(() => import('./pages/platform/journey/JourneyHome'));
// journey 的九页是具名导出，lazy 只认 default，故逐个转一次形。
const JourneyInvitations = lazy(() =>
  import('./pages/platform/journey/JourneyBeginnings').then((m) => ({ default: m.JourneyInvitations })),
);
const JourneyOnboarding = lazy(() =>
  import('./pages/platform/journey/JourneyBeginnings').then((m) => ({ default: m.JourneyOnboarding })),
);
const JourneyOoc = lazy(() =>
  import('./pages/platform/journey/JourneyBeginnings').then((m) => ({ default: m.JourneyOoc })),
);
const JourneyIfline = lazy(() =>
  import('./pages/platform/journey/JourneyStories').then((m) => ({ default: m.JourneyIfline })),
);
const JourneySubplot = lazy(() =>
  import('./pages/platform/journey/JourneyStories').then((m) => ({ default: m.JourneySubplot })),
);
const JourneyChapters = lazy(() =>
  import('./pages/platform/journey/JourneyConnections').then((m) => ({ default: m.JourneyChapters })),
);
const JourneyLive = lazy(() =>
  import('./pages/platform/journey/JourneyConnections').then((m) => ({ default: m.JourneyLive })),
);
const JourneySocial = lazy(() =>
  import('./pages/platform/journey/JourneyConnections').then((m) => ({ default: m.JourneySocial })),
);

// Mobile components（MobileShell 同 PlatformShell：壳保持静态）
import MobileShell from './components/MobileShell';
const MobileHome = lazy(() => import('./pages/MobileHome'));
const MobileChat = lazy(() => import('./pages/MobileChat'));
const MobileStory = lazy(() => import('./pages/MobileStory'));
const MobileBond = lazy(() => import('./pages/MobileBond'));
import { useSettingsStore } from './stores/useSettingsStore';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { isMobile, isTauriHost } from './utils/runtime';
import { applyPartnerStoreContent } from './utils/partnerStoreSync';
import './App.css';

function App() {
  const setWorksDirectory = useSettingsStore((s) => s.setWorksDirectory);
  const mobileEnv = isMobile();
  // 平台世界有自己的响应式外壳；不能因为窄屏就把 /platform/* 静默替换成旧移动端首页。
  const platformRoute = typeof window !== 'undefined' && window.location.pathname.startsWith('/platform');
  const tauriHost = isTauriHost();

  useEffect(() => {
    // Only invoke desktop setup commands on desktop
    if (!mobileEnv && tauriHost) {
      invoke<string>('get_workspace_dir', { dirType: 'articles' })
        .then((dir) => setWorksDirectory(dir))
        .catch((err) => console.error('Failed to initialize workspace directory:', err));
    }
  }, [mobileEnv, setWorksDirectory, tauriHost]);

  useEffect(() => {
    if (mobileEnv || !tauriHost) return;

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
  }, [mobileEnv, tauriHost]);

  return (
    <Router>
      <Routes>
        {mobileEnv && !platformRoute ? (
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
              <Route path="worlds/publish" element={<RequireAuth><WorldPublish /></RequireAuth>} />
              <Route path="my" element={<RequireAuth><MyWorlds /></RequireAuth>} />
              {/* 留存核心：以角色为轴（我的角色 / 一生档案）+ 跨世界背包 / 羁绊 */}
              <Route path="characters" element={<RequireAuth><MyCharacters /></RequireAuth>} />
              <Route path="characters/:cid" element={<RequireAuth><CharacterArchive /></RequireAuth>} />
              <Route path="backpack" element={<RequireAuth><Backpack /></RequireAuth>} />
              <Route path="bonds" element={<RequireAuth><Bonds /></RequireAuth>} />
              <Route path="journey" element={<RequireAuth><JourneyHome /></RequireAuth>} />
              <Route path="journey/onboarding" element={<RequireAuth><JourneyOnboarding /></RequireAuth>} />
              <Route path="journey/invitations" element={<RequireAuth><JourneyInvitations /></RequireAuth>} />
              <Route path="journey/ooc" element={<RequireAuth><JourneyOoc /></RequireAuth>} />
              <Route path="journey/iflines" element={<RequireAuth><JourneyIfline /></RequireAuth>} />
              <Route path="journey/subplot" element={<RequireAuth><JourneySubplot /></RequireAuth>} />
              <Route path="journey/social" element={<RequireAuth><JourneySocial /></RequireAuth>} />
              <Route path="journey/chapters" element={<RequireAuth><JourneyChapters /></RequireAuth>} />
              <Route path="journey/live" element={<RequireAuth><JourneyLive /></RequireAuth>} />
              <Route path="worlds/:id" element={<RequireAuth><WorldRoom /></RequireAuth>} />
              <Route path="worlds/:id/spectate" element={<RequireAuth><WorldSpectate /></RequireAuth>} />
              <Route path="reports" element={<RequireAuth><DailyReport /></RequireAuth>} />
              <Route path="reports/:id" element={<RequireAuth><DailyReport /></RequireAuth>} />
              {/* P4b 钱包 + P6 赛事房（独立平台页；本地页面不受影响） */}
              <Route path="wallet" element={<RequireAuth><Wallet /></RequireAuth>} />
              <Route path="arena/:worldId/host" element={<RequireAuth><ArenaHost /></RequireAuth>} />
              <Route path="arena/:worldId/spectate" element={<RequireAuth><ArenaSpectate /></RequireAuth>} />
              <Route path="arena/:worldId/replay" element={<RequireAuth><ArenaReplay /></RequireAuth>} />
            </Route>
          </>
        )}
      </Routes>
    </Router>
  );
}

export default App;
