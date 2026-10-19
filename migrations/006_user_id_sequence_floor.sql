-- 普通用户 user_id 从 NORMAL_USER_ID_START（100_000_000，src/config.rs）起发号。
--
-- 001_baseline 是 schema-only 导出，序列被还原成 START WITH 1：空库上服务启动先用显式
-- user_id=1 建了系统用户，第一个普通注册（含 privchat-application 启动时登记系统账号）
-- 再从序列取到 1，撞 privchat_users_pkey，应用拒绝启动。1 以上那段号也本该留给系统账号。
--
-- 只往上推、从不往回拨：已经发到 1 亿以上的库（生产）这里是空操作；
-- 可重复执行。
ALTER SEQUENCE public.privchat_users_user_id_seq START WITH 100000000;

SELECT setval(
    'public.privchat_users_user_id_seq',
    GREATEST(
        (SELECT last_value FROM public.privchat_users_user_id_seq),
        (SELECT COALESCE(MAX(user_id), 0) FROM public.privchat_users),
        100000000 - 1
    ),
    true
);
