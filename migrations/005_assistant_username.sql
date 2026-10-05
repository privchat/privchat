-- Assistant 系统账号的 username 改成简洁的 `assistant`（原 `_assistant_system_user`）。
--
-- user_id 不变，会话、消息、system_user_profile 都按 user_id 关联，不受影响。
-- privchat-application 的 assistant 模块启动时按 username 幂等 createUser：
-- 🔴 必须先跑本迁移，再部署改了 ASSISTANT_USERNAME 的 application，
--    否则启动会按新名字再建一个 assistant 账号。
--
-- 可重复执行（run_migrations.sh 每次全量重跑）：改过之后再跑是空操作。
-- 若 `assistant` 已被其他账号占用则中止，不覆盖任何人的 username。
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM public.privchat_users
        WHERE username = 'assistant'
          AND NOT (user_type = 1)
    ) THEN
        RAISE EXCEPTION 'username "assistant" is taken by a non-system user; rename aborted';
    END IF;

    IF EXISTS (SELECT 1 FROM public.privchat_users WHERE username = '_assistant_system_user')
       AND EXISTS (SELECT 1 FROM public.privchat_users WHERE username = 'assistant') THEN
        RAISE EXCEPTION 'both "_assistant_system_user" and "assistant" exist; resolve manually';
    END IF;

    -- 推进 sync_version：客户端按它增量同步用户资料，不推进就看不到新 username。
    UPDATE public.privchat_users
    SET username = 'assistant',
        updated_at = public.now_millis(),
        sync_version = nextval('public.privchat_user_entity_sync_version_seq'::regclass)
    WHERE username = '_assistant_system_user'
      AND user_type = 1;
END $$;
