-- 024: 群级内容保护 —— 禁止转发（MEDIA_REFERENCE_AND_FORWARD_SPEC §6.3）
--
-- 与 all_muted 同处：**DB 是真源**，server 重启后仍生效，不依赖可能丢失的内存缓存。
--
-- 语义：开启后，该群里的消息不允许被转发出去。服务端在创建副本**之前**拒绝，
-- 返回 FORWARDS_RESTRICTED（对齐 Telegram CHAT_FORWARDS_RESTRICTED）。
--
-- 🔴 已经合法完成的转发**不追溯删除**：事后开启保护只影响之后的转发。
-- 副本是目标会话里的独立消息，追溯删除等于让第三方的会话内容被源群单方面改写。

ALTER TABLE privchat_groups
    ADD COLUMN IF NOT EXISTS forbid_forward BOOLEAN NOT NULL DEFAULT false;
