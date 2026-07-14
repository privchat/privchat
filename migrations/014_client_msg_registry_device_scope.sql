-- 014_client_msg_registry_device_scope.sql
-- CODEX-8 幂等键 device 作用域（GA 前 P1，实为跨用户正确性缺陷）。
--
-- 缺口：privchat_client_msg_registry 以 local_message_id 单列为 PRIMARY KEY 做**全局**判重
--（表里有 sender_id 列但 check_duplicate 查询不用）。local_message_id 是客户端雪花，worker
-- 位仅 pid/启动时间各取 5bit（1024 组合、非稳定身份）——两个**不同用户**（或同账号两设备）
-- 撞出相同 local_message_id 时，后者被静默判重、拿到前者的 server_msg_id → 消息静默丢失。
--
-- 修复：幂等命名空间收敛为 (sender_id, device_id, local_message_id)：
--   - 跨用户天然隔离（sender_id 来自 JWT，服务端权威）；
--   - 同账号多设备互不干扰（device_id 来自认证会话，服务端权威，不信 payload）；
--   - 同设备重试仍精确幂等。
-- 历史行 device_id 回填 ''（升级窗口内跨版本重试的幂等性丢失，属可接受的标准折衷）。

ALTER TABLE privchat_client_msg_registry
    ADD COLUMN IF NOT EXISTS device_id varchar(128) NOT NULL DEFAULT '';

ALTER TABLE privchat_client_msg_registry
    DROP CONSTRAINT IF EXISTS privchat_client_msg_registry_pkey;

ALTER TABLE privchat_client_msg_registry
    ADD CONSTRAINT privchat_client_msg_registry_pkey
    PRIMARY KEY (sender_id, device_id, local_message_id);
