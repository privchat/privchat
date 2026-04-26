-- 014: v1.2 service user create 幂等改造
-- 目标：
--   1) 加 business_system_id 列（v1.2 service API 审计与列表过滤字段，非空时落库）
--   2) 放宽 username 为 NULLABLE（v1.2 起 phone-only / email-only / 占位 uid
--      创建路径合法；username 不再是 server 的强制身份字段）
--
-- 不需要做：
--   - user_type 列已存在（v1.0 起 schema 已包含 SMALLINT DEFAULT 0），无需新增
--   - phone UNIQUE 索引已存在（idx_privchat_users_phone partial index）
--   - username UNIQUE 约束在 PostgreSQL 中默认允许多个 NULL 值，无需重建索引
--
-- 历史数据：
--   - 现有 username 都是 NOT NULL 的合法值；DROP NOT NULL 不影响存量行
--   - 现有 business_system_id 视为 NULL（无业务系统标识）

ALTER TABLE privchat_users
    ADD COLUMN IF NOT EXISTS business_system_id VARCHAR(64);

ALTER TABLE privchat_users
    ALTER COLUMN username DROP NOT NULL;
