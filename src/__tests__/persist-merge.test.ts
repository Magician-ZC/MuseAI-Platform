/**
 * 🔴 持久化深合并（`src/stores/persistMerge.ts`）。
 *
 * 它修的是 zustand `persist` 的**浅**合并：给一个嵌套对象加字段之后，
 * 老用户盘上的数据 rehydrate 出来那个字段是 `undefined` 而不是默认值——
 * 而 TS 类型上它是必填的，读它的代码会当它存在。
 */
import { describe, expect, it } from 'vitest';
import { deepMergePersisted } from '../stores/persistMerge';

describe('deepMergePersisted', () => {
  it('🔴 嵌套对象里新加的字段必须回落到默认值（这是它存在的全部理由）', () => {
    const current = { settings: { a: 0, b: '新加的默认值' }, top: '默认' };
    const persisted = { settings: { a: 1 }, top: '盘上的' }; // 老版本：settings 没有 b
    const merged = deepMergePersisted(persisted, current);
    expect(merged).toEqual({ settings: { a: 1, b: '新加的默认值' }, top: '盘上的' });
  });

  it('多层嵌套一样补齐', () => {
    const current = { a: { b: { c: 1, d: 2 } } };
    const merged = deepMergePersisted({ a: { b: { c: 9 } } }, current);
    expect(merged).toEqual({ a: { b: { c: 9, d: 2 } } });
  });

  it('🔴 数组整体替换，绝不与默认值合并', () => {
    // 默认值里有两张预置卡，用户把它们删光了 —— 合并的话会**凭空变回来**。
    const current = { cards: [{ id: 'preset-1' }, { id: 'preset-2' }] };
    const merged = deepMergePersisted({ cards: [] }, current);
    expect(merged).toEqual({ cards: [] });

    const merged2 = deepMergePersisted({ cards: [{ id: 'mine' }] }, current);
    expect(merged2).toEqual({ cards: [{ id: 'mine' }] });
  });

  it('null 是有意义的值，以盘上那份为准；标量同理', () => {
    const current = { pick: { id: 'x' }, n: 3 };
    expect(deepMergePersisted({ pick: null, n: 0 }, current)).toEqual({ pick: null, n: 0 });
  });

  it('盘上不是对象（首次运行 / 结构被改坏）→ 用默认值，不猜', () => {
    const current = { a: 1 };
    expect(deepMergePersisted(undefined, current)).toEqual(current);
    expect(deepMergePersisted('坏了', current)).toEqual(current);
    expect(deepMergePersisted(null, current)).toEqual(current);
  });

  it('不改动传入的两个对象（persist 会在 rehydrate 期间复用它们）', () => {
    const current = { s: { a: 1, b: 2 } };
    const persisted = { s: { a: 9 } };
    deepMergePersisted(persisted, current);
    expect(current).toEqual({ s: { a: 1, b: 2 } });
    expect(persisted).toEqual({ s: { a: 9 } });
  });
});

/**
 * 🔴 **每一个持久化 store 都必须显式声明合并语义。**
 *
 * 默认的浅合并会让嵌套对象里新加的字段在老用户盘上变成 `undefined`
 * （`migrate` 挡不住——它只在 `version` 不匹配时才跑）。
 *
 * 判据从**源码目录实际存在的 store** 出发，不是手列一张表：
 * 新加一个持久化 store 就自动进入判据，忘了声明 `merge` 就红。
 * `docs/VALIDATION.md` §3.8.1：遗漏必须往红的方向失败。
 */
describe('持久化 store 的合并语义', () => {
  const files = import.meta.glob('../stores/*.ts', { query: '?raw', import: 'default', eager: true }) as Record<string, string>;

  it('🔴 每个用了 persist 的 store 都声明了 merge', () => {
    const offenders: string[] = [];
    let scanned = 0;
    for (const [path, src] of Object.entries(files)) {
      if (path.endsWith('.test.ts') || path.endsWith('/diskStorage.ts') || path.endsWith('/persistMerge.ts')) continue;
      if (!/\bpersist\s*(<[^>]*>)?\s*\(/.test(src)) continue;
      scanned += 1;
      if (!/^\s*merge:/m.test(src)) offenders.push(path);
    }
    expect(scanned).toBeGreaterThanOrEqual(15);
    expect(offenders, `🔴 这些 store 没声明 merge —— 默认浅合并会让嵌套对象里新加的字段\n` +
      `在老用户盘上变成 undefined，而 TS 类型上它是必填的。\n` +
      `请加 merge: (p, c) => deepMergePersisted(p, c)（或写清为什么这个 store 不需要）。`).toEqual([]);
  });
});
