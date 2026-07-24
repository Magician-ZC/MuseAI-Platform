-- MuseAI 平台库 0018：内容风控申诉复审（moderation_appeals + audit_queue.reject_reason）。
-- 背景：机审 Rejected 不产生 audit_queue 行（safety::moderate_and_queue 仅 Pending 入队），
-- 申诉必须独立成表挂在「主体」上，而非挂在审核队列行上。
-- 双库可跑：禁方言；id TEXT；时间 BIGINT ms；status 文本枚举。

CREATE TABLE moderation_appeals (
  id TEXT PRIMARY KEY,
  subject_kind TEXT NOT NULL,                    -- character（后续可扩 world_template 等）
  subject_id TEXT NOT NULL,
  owner_id TEXT NOT NULL,                        -- 申诉发起人（主体 owner）
  appeal_text TEXT NOT NULL,                     -- 申诉正文（trim 后 1..=500 字符）
  status TEXT NOT NULL DEFAULT 'pending',        -- pending / upheld（维持原判）/ overturned（改判）
  resolution_reason TEXT,                        -- 复审结论理由（resolve 时必填）
  reviewer_id TEXT,                              -- 复审人
  created_at BIGINT NOT NULL,
  resolved_at BIGINT
);
-- 每主体终身一次申诉：同 (subject_kind, subject_id) 唯一，重复提交由唯一索引冲突 → 409。
CREATE UNIQUE INDEX idx_moderation_appeals_subject ON moderation_appeals(subject_kind, subject_id);
CREATE INDEX idx_moderation_appeals_status ON moderation_appeals(status);

-- 人审驳回理由落队列行（此前只进 audit_logs，用户侧不可见）；历史行留 NULL。
ALTER TABLE audit_queue ADD COLUMN reject_reason TEXT;
