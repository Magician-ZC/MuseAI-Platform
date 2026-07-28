/**
 * 🔴 **持久化状态的深合并**——zustand `persist` 的默认合并是**浅**的。
 *
 * # 它修的是什么
 *
 * 默认 `merge` 等价于 `{ ...initialState, ...persisted }`。于是：
 *
 * | 改动 | 老用户盘上 rehydrate 之后 |
 * |---|---|
 * | 加一个**顶层**字段 | ✅ 保留默认值 |
 * | 给一个**嵌套对象**加字段 | 🔴 **整个嵌套对象被盘上那份替换 → 新字段是 `undefined`** |
 *
 * 而 TS 类型上那个字段是必填的，读它的代码会当它存在——于是拿到 `undefined`
 * 去做 `.map` / 比较 / 拼串。症状五花八门，**没有一条会指向「持久化合并」**。
 *
 * ⚠️ `migrate` **挡不住这一类**：它只在 `version` 不匹配时才跑。
 * 「给嵌套对象加了个字段但没 bump version」是最自然、也最容易发生的改法，
 * 那种情况下 `migrate` 一次都不会被调用。
 *
 * # 边界：数组**整体替换**，不逐元素合并
 *
 * 数组在这些 store 里表示的是「用户当前拥有的那一批东西」（角色卡、预设、会话…），
 * 把默认值里的元素合并进去等于**凭空给用户变出他删掉的东西**。
 * 只有普通对象递归，数组与标量一律以盘上那份为准。
 *
 * `null` 也以盘上那份为准（它是一个**有意义的值**，不是缺失）。
 */
function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

/** 递归合并：盘上有的以盘上为准，盘上缺的用默认值补齐。 */
export function deepMergePersisted<T>(persisted: unknown, current: T): T {
  if (!isPlainObject(persisted) || !isPlainObject(current)) {
    // 盘上不是对象（首次运行为 undefined，或结构被改坏）→ 用默认值，不猜。
    return isPlainObject(persisted) ? (persisted as T) : current;
  }
  const out: Record<string, unknown> = { ...current };
  for (const [k, v] of Object.entries(persisted)) {
    const cur = (current as Record<string, unknown>)[k];
    out[k] = isPlainObject(v) && isPlainObject(cur) ? deepMergePersisted(v, cur) : v;
  }
  return out as T;
}
