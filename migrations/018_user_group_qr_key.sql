-- 018_user_group_qr_key.sql
-- QR_CODE_SPEC v1.3 — Step 0 持久化层
--
-- 给 privchat_users / privchat_groups 各加一列 qr_key (VARCHAR(16) UNIQUE NOT NULL)，
-- 用作个人名片 / 群二维码的永久 opaque token。
--
-- 三阶段：
--   Phase 1 — ALTER TABLE 加 nullable 列（不影响在线 INSERT）
--   Phase 2 — 用 pgcrypto::gen_random_bytes(8) 回填全部存量行；hex 编码得到 16 位
--             [0-9a-f] 字符串，是 base62 子集，满足 spec 的 URL-safe 与 16-char 规则。
--             收尾逐行 UPDATE + retry on unique_violation 防御性兜底。
--   Phase 3 — SET NOT NULL + CREATE UNIQUE INDEX
--
-- 注意：
--   * 运行时生成的 qr_key 由 Rust 侧 `crate::rpc::qr::key::generate_qr_key()` 用 base62
--     全字母表（[a-zA-Z0-9]）打出来，~95 bits 熵；本 migration 仅给存量行打 64 bits
--     hex 一次性回填，够用。
--   * 不在 ALTER TABLE 阶段直接写 `DEFAULT random_qr_key()` —— Postgres 的 DEFAULT 在
--     ALTER 时会逐行 rewrite，但不保证 UNIQUE，碰撞处理也不灵活。
--   * 整段 migration 在自动事务内执行（main.rs::run_migrate 用 `sqlx::raw_sql`），
--     失败回滚。

CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- Phase 1: 加 nullable 列
ALTER TABLE privchat_users  ADD COLUMN qr_key VARCHAR(16);
ALTER TABLE privchat_groups ADD COLUMN qr_key VARCHAR(16);

-- Phase 2: 回填。逐行 UPDATE + 异常重试，本阶段还没加 UNIQUE，
-- WHERE qr_key IS NULL 保证幂等（重跑 migration 不会覆盖已写值，虽然 runner
-- 已经按 name 跳过这一段，但 belt-and-suspenders）。
DO $$
DECLARE
    r record;
    attempts integer;
BEGIN
    -- 用户
    FOR r IN SELECT user_id FROM privchat_users WHERE qr_key IS NULL LOOP
        attempts := 0;
        LOOP
            attempts := attempts + 1;
            BEGIN
                UPDATE privchat_users
                SET qr_key = encode(gen_random_bytes(8), 'hex')
                WHERE user_id = r.user_id;
                EXIT;
            EXCEPTION WHEN unique_violation THEN
                IF attempts >= 5 THEN
                    RAISE EXCEPTION 'qr_key backfill failed for user_id=% after 5 attempts', r.user_id;
                END IF;
            END;
        END LOOP;
    END LOOP;

    -- 群组
    FOR r IN SELECT group_id FROM privchat_groups WHERE qr_key IS NULL LOOP
        attempts := 0;
        LOOP
            attempts := attempts + 1;
            BEGIN
                UPDATE privchat_groups
                SET qr_key = encode(gen_random_bytes(8), 'hex')
                WHERE group_id = r.group_id;
                EXIT;
            EXCEPTION WHEN unique_violation THEN
                IF attempts >= 5 THEN
                    RAISE EXCEPTION 'qr_key backfill failed for group_id=% after 5 attempts', r.group_id;
                END IF;
            END;
        END LOOP;
    END LOOP;
END $$;

-- Phase 3: 强制 NOT NULL + UNIQUE
ALTER TABLE privchat_users  ALTER COLUMN qr_key SET NOT NULL;
ALTER TABLE privchat_groups ALTER COLUMN qr_key SET NOT NULL;

CREATE UNIQUE INDEX ux_privchat_users_qr_key  ON privchat_users  (qr_key);
CREATE UNIQUE INDEX ux_privchat_groups_qr_key ON privchat_groups (qr_key);

COMMENT ON COLUMN privchat_users.qr_key  IS 'QR_CODE_SPEC v1.3: 个人名片码 opaque token, 16 chars base62, UNIQUE, 永久，用户可主动 refresh 旋转';
COMMENT ON COLUMN privchat_groups.qr_key IS 'QR_CODE_SPEC v1.3: 群二维码 opaque token, 16 chars base62, UNIQUE, 永久，Owner/Admin 可 refresh 旋转';
