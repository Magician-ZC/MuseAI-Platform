-- MuseAI 平台库 0046：**语义分类异步复核的运行台账**（总规格 §15 运行时内容安全**第 3 层**）。
--
-- 落地的是 `server/src/safety/mod.rs` 挂了很久的 `TODO(§15-L3)`，以及
-- `providers::ModerationProvider::check_text` 文档里同名的那条 TODO。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 🔴 先说清楚这张表**不**证明什么
-- ═══════════════════════════════════════════════════════════════════════════
-- `ModerationProvider` 当前的唯一实现是 **Dev 桩**（`providers::DevModeration`：一张小关键词表，
-- 其余一律直过）。所以第 3 层接通之后**仍然拦不住任何东西**——本批次交付的是**管线**，不是防线。
--
-- 这不是一句写在注释里就算数的免责声明：`provider_stub` 是本表的**一等列**，
-- 每一行都随数据带着「这行数出自桩」这个事实，运营面读数时它与数字一起出现
-- （范式抄 `slo::quality::QualitySource::SimulatedStub` 把桩的事实做成随报告 JSON 走的字段）。
-- 谁把这张表的数字贴进评审材料，就必然同时看见 `provider_stub = 1`。
--
-- 🔴 因此**任何文档、看板、报告都不得**据此表述为「五层漏斗已完整」「内容安全已就绪」。
-- 接真实服务商 = 换 `ModerationProvider` 实现并把 `is_dev_stub()` 覆写为 false，
-- 那一刻本列自动变 0，届时才谈得上「防线」。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 为什么要一张表，而不是只写日志
-- ═══════════════════════════════════════════════════════════════════════════
-- `docs/VALIDATION.md` §2 T5 有一条门槛：「**内容审核成本 ≤ 生成成本的 5%**」。
-- 这条门槛此前**没有任何数据源**——审核侧一次调用都没有被计过数。本表是那个分子侧的口径起点：
-- 每次复核记下送审条数、送审字符数、命中数、重试次数与耗时；分母侧（生成成本）
-- 一直都在 `world_ticks.cost_tokens` 里。
--
-- ⚠️ 但**比值本身现在仍算不出来**，本迁移不假装能算：`check_text` 只回裁决，不回 token 也不回费用，
-- 而桩的调用成本恒为 0。所以本表交付的是**调用量与送审字符数**，换算成钱要等真实 provider 的计价口径。
-- 这一点同样写进运营面响应（`cost.ratioAvailable = false` + `why`），不靠人记。
--
-- ═══════════════════════════════════════════════════════════════════════════
-- 一行 = 一次**尝试**（不是一拍）
-- ═══════════════════════════════════════════════════════════════════════════
-- provider 超时/报错会重试（退避、次数有上限、参数化）。**每次尝试都真实烧调用**，
-- 所以粒度必须是尝试而不是拍——按拍记会把重试的开销系统性地记漏，
-- 而「审核成本失控」恰恰最可能来自重试风暴。唯一索引 `(world_id, tick_no, attempt)`
-- 让重复投递自然幂等（写入侧 `ON CONFLICT DO NOTHING`）。
--
-- 🔴 本表**只记账，不存内容**：没有任何一列存事件正文。正文的唯一事实源仍是 `world_events`，
-- 而第 3 层对 `world_events` 的唯一写入是把 `moderation` 从 'approved' **收紧**为 'pending'
-- （SET 列表里只有这一列，WHERE 还钉着 `moderation='approved'`）——
-- 正文一个字节不动（§0.3 公共事实不可回滚），由红线用例逐字节守死。

CREATE TABLE safety_recheck_runs (
  id TEXT PRIMARY KEY,

  -- 复核对象：某个世界的某一拍。载荷只有这两个字段，事件清单每次从 `world_events` 现查——
  -- 于是任务天然幂等、可重放，且已被第 2 层拦下的事件不会被重复送审（它们已不是 'approved'）。
  world_id TEXT NOT NULL,
  tick_no BIGINT NOT NULL,
  -- 第几次尝试（从 1 起）。见上：粒度是尝试，不是拍。
  attempt BIGINT NOT NULL,

  -- ── 送审口径（公开全量 / 私有抽样，抽样率参数化且确定性） ──────────────
  -- candidates = 本次可送审的条数（moderation 仍为 'approved' 的）；
  -- checked     = 抽样后真正调用了 check_text 的条数。公开档默认全量 ⇒ 两者相等。
  public_candidates BIGINT NOT NULL DEFAULT 0,
  public_checked BIGINT NOT NULL DEFAULT 0,
  private_candidates BIGINT NOT NULL DEFAULT 0,
  private_checked BIGINT NOT NULL DEFAULT 0,
  -- 本次生效的抽样率快照（**万分比整数**，10000 = 全量）。
  -- 落库而不是只读 env：事后复盘「那天为什么只查了三成」必须能查到当时的配置，
  -- 而 env 是进程级的、改了不留痕。整数是刻意的——确定性契约禁浮点 RNG。
  public_sample_bp BIGINT NOT NULL DEFAULT 0,
  private_sample_bp BIGINT NOT NULL DEFAULT 0,
  -- 送审文本总字符数（成本的 provider 无关近似量：token 计价要等真实 provider）。
  chars_checked BIGINT NOT NULL DEFAULT 0,

  -- ── 处置结果 ────────────────────────────────────────────────────────────
  -- 收紧条数（approved → pending/rejected）。**只收紧、不放宽**：本层永不写 'approved'。
  tightened BIGINT NOT NULL DEFAULT 0,
  -- provider 报错/超时的条数（本次尝试内）。
  provider_errors BIGINT NOT NULL DEFAULT 0,
  -- 其中因**重试预算耗尽**而按 fail-closed 收紧的条数。
  -- 🔴 fail-closed 的方向与 `MUSE_SAFETY_LEXICON` 的 fail-safe（默认「继续过滤」）自洽：
  -- 审核链自身的故障绝不允许转化为放行，否则「打掉审核 provider」就成了绕过第 3 层的手段。
  failed_closed BIGINT NOT NULL DEFAULT 0,
  -- 其中在**直播播出水位线之前**拦下的条数（§15 第 4 层给第 3 层留的那个窗口是否够用的度量，
  -- 口径同 `live_withholds.preemptive`）。⚠️ 它只说明**直播观众**没看见；
  -- 世界成员的读取面从不延迟，对他们而言第 3 层恒为事后收紧。
  intercepted_before_broadcast BIGINT NOT NULL DEFAULT 0,

  -- ── 运行元信息 ──────────────────────────────────────────────────────────
  -- 本次尝试耗时（毫秒）。BIGINT 毫秒，双库可移植（不用 strftime / date_trunc / NOW()）。
  latency_ms BIGINT NOT NULL DEFAULT 0,
  -- 🔴 1 = 这批数出自 Dev 桩。见文件头。布尔一律 INTEGER 0/1（双库可移植）。
  provider_stub INTEGER NOT NULL DEFAULT 1,
  -- done（本次尝试全部有裁决）/ retry_scheduled（有报错，已按退避重排）/
  -- failed_closed（重试预算耗尽，按 fail-closed 收紧）/ skipped（开关关闭或无候选）
  outcome TEXT NOT NULL DEFAULT 'done',

  created_at BIGINT NOT NULL
);

-- 幂等：同一拍的同一次尝试只留一行（重复投递 → ON CONFLICT DO NOTHING）。
CREATE UNIQUE INDEX idx_safety_recheck_runs_unique
  ON safety_recheck_runs(world_id, tick_no, attempt);
-- 运营面按时间窗聚合（成本读数）。次级键保证全序——PG 对并列行不保证顺序。
CREATE INDEX idx_safety_recheck_runs_created
  ON safety_recheck_runs(created_at, id);
-- 按世界排查（「这个世界的复核为什么一直在重试」）。
CREATE INDEX idx_safety_recheck_runs_world
  ON safety_recheck_runs(world_id, tick_no, attempt);
