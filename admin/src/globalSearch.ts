// 后台顶栏全局搜索的分发规则。
//
// ══════════════════════════════════════════════════════════════════════════════
// 它此前是一个**完全没有 onSearch 的输入框**
// ══════════════════════════════════════════════════════════════════════════════
// `App.tsx` 顶栏那个 `<Input placeholder="搜索世界、房间、角色、事件ID…">` 从落地起就没有绑过
// 任何回调：敲字、回车、点放大镜，什么都不会发生。这比没有搜索框更糟——它长期看起来像已经接好了
// （VALIDATION §3.60 那句「还没做和做完了但点不到，在用户那里是同一件事」的又一例）。
//
// ══════════════════════════════════════════════════════════════════════════════
// 🔴 这是**分发**不是检索：零后端改动，且刻意不假装自己能搜内容
// ══════════════════════════════════════════════════════════════════════════════
// 后台今天只有一个端点支持文本检索（`GET /admin/users?query=`，Users 页已经会读 URL 上的
// `?query=`）。其余模块都只有按状态/游标翻页，没有 `q` 参数。
// 所以本函数做的是两件确定的事：
//   ① **按 id 前缀认领**——全仓 id 都是 `db::new_id()` 产的 `前缀_hex`，前缀是确定的、可枚举的，
//      认出来就把人送到真正处理这类主体的那个模块；
//   ② **其余文本**（手机号 / 邮箱 / 昵称）交给唯一真的能检索的 `users?query=`。
// 认不出来又不像账号的，返回 `null` —— 调用方据此给一句「没认出来」，而不是静默什么都不做。
//
// ⚠️ **不在这里造一个"全都搜一遍"的聚合端点**：那需要每个模块都补 `?q=`，且要先定义
// 「搜到多个主体怎么排序」——是产品决定，不是补一个输入框的活。

/** 分发结果：去哪个模块、URL 上带什么、给用户看什么解释。 */
export interface SearchTarget {
  /** RBAC 模块 key（`MODULES` 里的那个），调用方必须先过 `canAccess` 再跳。 */
  module: string;
  /** 目标路径（不含 `design=preview` 之类的既有 query，由调用方合并）。 */
  path: string;
  /** 人话解释，用于「你搜的这个归 X 模块」的提示。 */
  what: string;
}

/**
 * id 前缀 → 处理它的后台模块。
 *
 * 🔴 这是一张**手工维护的表**，会过期——新加一类主体而忘了登记，搜索会把它落到 users 兜底上。
 * 之所以还是手工列：前缀常量散落在各模块的 `new_id("xx")` 调用里，没有一处集中登记，
 * 而在**后台前端**去反推服务端的 id 生成点，会造出一份比这张表更容易漂的东西。
 * 漂移方向是安全的：认不出 = 落到 users 或返回 null，不会把人送进一个错的处置页。
 */
const PREFIX_ROUTES: { prefix: string; module: string; path: string; what: string }[] = [
  { prefix: 'wld', module: 'worlds', path: '/worlds', what: '世界实例' },
  { prefix: 'wtpl', module: 'worlds', path: '/worlds?view=templates', what: '创作者发布的世界模板' },
  { prefix: 'tpl', module: 'worlds', path: '/worlds?view=templates', what: '运营建的世界模板' },
  { prefix: 'cchar', module: 'audit', path: '/audit', what: '云端角色卡' },
  { prefix: 'aq', module: 'audit', path: '/audit', what: '审核队列条目' },
  { prefix: 'apl', module: 'audit', path: '/audit', what: '审核申诉' },
  { prefix: 'srp', module: 'social', path: '/social', what: '社交举报' },
  { prefix: 'dr', module: 'tickets', path: '/tickets', what: '数据请求' },
  { prefix: 'risk', module: 'risk', path: '/risk', what: '风控事件' },
  { prefix: 'order', module: 'economy', path: '/economy', what: '订单' },
  { prefix: 'user', module: 'users', path: '/users', what: '账号' },
];

/** 看起来像账号标识（手机号 / 邮箱 / 昵称片段）→ 交给唯一真能检索的 users。 */
function looksLikeAccount(term: string): boolean {
  return term.includes('@') || /^[0-9+\-\s]{6,}$/.test(term);
}

export function resolveSearch(raw: string): SearchTarget | null {
  const term = raw.trim();
  if (!term) return null;

  const sep = term.indexOf('_');
  if (sep > 0) {
    const prefix = term.slice(0, sep);
    const hit = PREFIX_ROUTES.find((r) => r.prefix === prefix);
    if (hit) {
      // users 是唯一能按 id 精确检索的，顺手把 id 也带过去；其余模块只做导航。
      const path = hit.module === 'users' ? `/users?query=${encodeURIComponent(term)}` : hit.path;
      return { module: hit.module, path, what: hit.what };
    }
  }

  if (looksLikeAccount(term)) {
    return { module: 'users', path: `/users?query=${encodeURIComponent(term)}`, what: '账号' };
  }
  return null;
}
