// 后台 RBAC 前端映射（#9 纵深防御 + UX）。
//
// 唯一权威是后端 server/src/admin_api 各 handler 里的 require_role；本表严格对齐每个模块
// 主读端点的 require_role 白名单，只做「收敛可见模块 + 越权提示」的前端纵深，不放宽任何权限。
//   users    GET /admin/users            → require_role(support)                     ⇒ support
//   audit    GET /admin/audit-queue      → require_role(reviewer)                    ⇒ reviewer
//   worlds   GET /admin/worlds           → require_role(operator)                    ⇒ operator
//   economy  GET /admin/economy/overview → require_role(finance)                     ⇒ finance
//   metrics  GET /admin/metrics/overview → require_role(operator, finance)           ⇒ operator, finance
//   prompts  GET /admin/prompts          → require_role(operator)                    ⇒ operator
//   risk     GET /admin/risk-events      → require_role(operator, reviewer, support) ⇒ operator, reviewer, support
//   tickets  GET /admin/data-requests    → require_role(support)                     ⇒ support
// admin 为超级角色，可见全部（对齐 require_role 中 role=="admin" 放行一切）。
//
// 等价的按角色视图：
//   admin    全部八模块
//   operator 世界运营 / 数据看板 / 模型与 Prompt / 风控（只读）
//   reviewer 内容审核 / 风控（只读）
//   support  用户管理 / 客服与工单 / 风控（只读）
//   finance  经济运营 / 数据看板（只读）

export type AdminRole = 'admin' | 'operator' | 'reviewer' | 'support' | 'finance';

export interface AdminModule {
  key: string;
  label: string;
  /** 除 admin 外可访问该模块的角色（对齐后端 require_role 白名单）。 */
  roles: AdminRole[];
}

/** 八大模块（顺序即菜单顺序，也决定各角色的默认落地模块）。 */
export const MODULES: AdminModule[] = [
  { key: 'users', label: '用户管理', roles: ['support'] },
  { key: 'audit', label: '内容审核', roles: ['reviewer'] },
  { key: 'worlds', label: '世界运营', roles: ['operator'] },
  { key: 'economy', label: '经济运营', roles: ['finance'] },
  { key: 'metrics', label: '数据看板', roles: ['operator', 'finance'] },
  { key: 'prompts', label: '模型与 Prompt', roles: ['operator'] },
  { key: 'risk', label: '风控', roles: ['operator', 'reviewer', 'support'] },
  { key: 'tickets', label: '客服与工单', roles: ['support'] },
];

const ROLE_LABEL: Record<string, string> = {
  admin: '超级管理员',
  operator: '运营',
  reviewer: '审核',
  support: '客服',
  finance: '财务',
};

/** 角色 → 中文名（未知角色原样回显）。 */
export function roleLabel(role: string | null): string {
  if (!role) return '未知角色';
  return ROLE_LABEL[role] ?? role;
}

/** 角色是否可访问某模块（admin 全放行；其余按白名单）。后端仍会二次强制校验。 */
export function canAccess(role: string | null, moduleKey: string): boolean {
  if (!role) return false;
  if (role === 'admin') return true;
  const m = MODULES.find((x) => x.key === moduleKey);
  return !!m && m.roles.includes(role as AdminRole);
}

/** 该角色可见的模块列表（保持 MODULES 顺序）。 */
export function visibleModules(role: string | null): AdminModule[] {
  return MODULES.filter((m) => canAccess(role, m.key));
}

/** 该角色登录后的默认落地模块 key（第一个可见模块；无可见模块则 null）。 */
export function firstModuleKey(role: string | null): string | null {
  return visibleModules(role)[0]?.key ?? null;
}
