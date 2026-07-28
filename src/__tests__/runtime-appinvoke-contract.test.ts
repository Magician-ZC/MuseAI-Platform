import { describe, expect, it } from 'vitest';
// Vite 的 `?raw` 导入：把源码当字符串读进来。
// ⚠️ 刻意**不用** `node:fs` —— 本项目的 tsconfig 不含 Node types（`lib` 只有 ES2020/DOM），
// 用它会让 `npm run build`（tsc + vite build）挂掉，而那是 CI 的 frontend job。
// 第一版就是这么写的，tsc 当场报了三条 TS2307/TS2304。
import runtimeSrc from '../utils/runtime.ts?raw';

/**
 * 🔴 `appInvoke` 的三处手工同步（CLAUDE.md 明写）：
 * Tauri command 注册（`lib.rs`）+ `mobile_server.rs` 的 axum 路由 + `appInvoke` 的 switch 分支。
 *
 * 本文件钉住其中**前端这一侧**的两条不变式。TypeScript 管不到它们：
 * `switch (cmd)` 不是穷尽检查，少一个 `case` 照样编译过，然后在手机上运行时抛异常。
 */
describe('appInvoke 的类型表与 switch 分支必须一致', () => {
  const src = runtimeSrc;

  /** `AppInvokeCommands` 里声明的命令名。 */
  const declared = (): string[] => {
    const m = src.match(/(?:export )?(?:type|interface) AppInvokeCommands\s*=?\s*\{/);
    if (!m || m.index === undefined) throw new Error('找不到 AppInvokeCommands 定义');
    let depth = 0;
    let end = m.index + m[0].length - 1;
    for (let i = end; i < src.length; i += 1) {
      if (src[i] === '{') depth += 1;
      else if (src[i] === '}') {
        depth -= 1;
        if (depth === 0) { end = i; break; }
      }
    }
    const body = src.slice(m.index, end);
    return [...body.matchAll(/^ {2}(\w+):/gm)].map((x) => x[1]);
  };

  const cases = (): string[] => [...src.matchAll(/case '(\w+)':/g)].map((x) => x[1]);

  it('声明支持的命令，switch 里必须都有分支', () => {
    const missing = declared().filter((c) => !cases().includes(c));
    expect(
      missing,
      `🔴 这些命令进了 AppInvokeCommands（= 宣告「手机端也支持」），但 switch 里没有分支：${missing.join(', ')}\n` +
        '手机端调它们会**类型检查通过、运行时抛异常**（default 分支 throw）。\n' +
        '要么补上 switch 分支 + mobile_server.rs 的路由，要么把它从类型表里删掉、改用直接 invoke。\n' +
        '⚠️ 别无脑补分支：`read_file` 曾在表里而无分支，补它意味着在局域网上开任意文件读取。',
    ).toEqual([]);
  });

  it('switch 里的分支必须都在类型表里声明', () => {
    const extra = cases().filter((c) => !declared().includes(c));
    expect(extra, `🔴 switch 有分支但类型表没声明：${extra.join(', ')}——调用方拿不到类型`).toEqual([]);
  });

  it('解析器本身没坏（否则上面两条会静默变成恒真）', () => {
    expect(declared().length).toBeGreaterThan(10);
    expect(cases().length).toBeGreaterThan(10);
  });

  /**
   * 🔴 `read_file` 读**任意路径**，永远不得进 `appInvoke` 的类型表。
   *
   * 它进表就等于宣告手机端支持，而支持就要在 `mobile_server.rs` 上开对应路由——
   * 那是在局域网上开任意文件读取（`~/.ssh/id_rsa` 之类）。
   * 2026-07-28 它确实在表里躺着（无分支、当前不可达），本条是为了它别再回去。
   */
  it('read_file 永远不进 appInvoke 的类型表', () => {
    expect(
      declared().includes('read_file'),
      '🔴 `read_file` 回到了 appInvoke 类型表——它读任意路径，进表 = 宣告手机端支持 = 要在局域网开任意文件读取。桌面专用命令请直接 invoke。',
    ).toBe(false);
  });
});
