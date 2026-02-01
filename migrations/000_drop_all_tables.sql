-- =====================================================
-- PrivChat 数据库清理脚本
-- 功能: 删除所有现有表（用于开发环境重建）
-- 警告: 此脚本会删除所有数据，仅用于开发环境！
-- =====================================================

-- 删除所有表（按依赖顺序）
DROP TABLE IF EXISTS privchat_login_logs CASCADE;
DROP TABLE IF EXISTS privchat_offline_message_queue CASCADE;
DROP TABLE IF EXISTS privchat_client_msg_registry CASCADE;
DROP TABLE IF EXISTS privchat_commit_log CASCADE;
DROP TABLE IF EXISTS privchat_channel_pts CASCADE;
DROP TABLE IF EXISTS privchat_user_last_seen CASCADE;
DROP TABLE IF EXISTS privchat_device_sync_state CASCADE;
DROP TABLE IF EXISTS privchat_user_channels CASCADE;
DROP TABLE IF EXISTS privchat_read_receipts CASCADE;
DROP TABLE IF EXISTS privchat_messages CASCADE;
DROP TABLE IF EXISTS privchat_channel_participants CASCADE;
DROP TABLE IF EXISTS privchat_channels CASCADE;
DROP TABLE IF EXISTS privchat_group_members CASCADE;
DROP TABLE IF EXISTS privchat_groups CASCADE;
DROP TABLE IF EXISTS privchat_blacklist CASCADE;
DROP TABLE IF EXISTS privchat_friendships CASCADE;
DROP TABLE IF EXISTS privchat_devices CASCADE;
DROP TABLE IF EXISTS privchat_user_settings CASCADE;
DROP TABLE IF EXISTS privchat_users CASCADE;

-- 文件上传记录表及序列
DROP TABLE IF EXISTS privchat_file_uploads CASCADE;
DROP SEQUENCE IF EXISTS privchat_file_uploads_id_seq CASCADE;

-- 删除辅助函数（如果存在）
DROP FUNCTION IF EXISTS now_millis() CASCADE;
DROP FUNCTION IF EXISTS update_updated_at_column() CASCADE;

-- =====================================================
-- 完成
-- =====================================================
