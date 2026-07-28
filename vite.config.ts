import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],
  test: {
    globals: true,
    environment: "jsdom",
    setupFiles: "./src/test/setup.ts",
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
    testTimeout: 15000,
    // 限制并发 worker：满核并发下 jsdom 重渲染测试（Settings/Markdown/Background 等）
    // 会因 CPU 抢占撞上 15s 超时而 flaky。限到半数核心让每个 worker 有足够 CPU。
    maxWorkers: "50%",
    // 🔴 **让 `admin/` 的组件能在这里被测**（docs/VALIDATION.md §3.47 欠账 A5）。
    //
    // `admin/` 没有自己的测试基建，而它的 7 个依赖（antd / echarts / react-router-dom…）
    // 根目录**一个都不缺**——所以后台组件可以直接跑在这套 vitest 下，一个新依赖都不用加。
    //
    // 唯一的障碍是 **React 有两份**：测试从根 `node_modules` 取，
    // 而 `admin/src/**` 与 `admin/node_modules/antd` 从 admin 那份取。
    // 两个 React 实例 → hooks 拿到 null context（症状是
    // `Cannot read properties of null (reading 'useContext')`，与业务代码无关）。
    // 这里把它们钉到同一份。
    //
    // ⚠️ **只在 `test` 段里做**：主应用的 dev/build 完全不受影响
    //（那两个包在生产构建里本来也各自独立，不该被测试的需要牵动）。
    alias: [
      // 逐个把 `admin/node_modules` 里的**共享包**钉到根目录那一份。
      // 症状是 `Cannot read properties of null (reading 'useContext')`——
      // antd 从 admin 那份 require React，而组件树跑在根那份 React 上，两个实例互不认识。
      ...["react", "react-dom", "antd", "@ant-design/icons", "echarts", "echarts-for-react", "react-router-dom"].map(
        (pkg) => ({
          find: new RegExp(`^${pkg.replace("/", "\\/")}(\\/.*)?$`),
          replacement: fileURLToPath(new URL(`./node_modules/${pkg}`, import.meta.url)) + "$1",
        }),
      ),
    ],
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
