-- =====================================================
-- PrivChat 数据库迁移
-- 版本: 001 (重构版 - 使用 uint64 ID)
-- 功能: 创建完整的 PrivChat 数据库表结构
-- 设计原则:
--   1. 所有表使用 privchat_ 前缀（PrivChat 专用表）
--   2. 所有时间字段使用 BIGINT 存储毫秒时间戳
--   3. 消息表按月分区（PostgreSQL 分区特性）
--   4. 使用 JSONB 存储灵活元数据
--   5. 核心业务ID使用 BIGINT/BIGSERIAL（uint64）
--   6. 消息ID使用 Snowflake（由应用层生成）
--   7. 设备ID保留 UUID（非核心路径）
-- =====================================================

-- =====================================================
-- 辅助函数
-- =====================================================

-- 创建时间戳生成函数（返回毫秒时间戳）
CREATE OR REPLACE FUNCTION now_millis() RETURNS BIGINT AS $$
    SELECT (extract(epoch from now()) * 1000)::BIGINT;
$$ LANGUAGE SQL IMMUTABLE;

-- 更新 updated_at 字段的触发器函数
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = now_millis();
    RETURN NEW;
END;
$$ language 'plpgsql';

-- =====================================================
-- PrivChat 核心表
-- =====================================================

-- =====================================================
-- 1. 用户表 (privchat_users)
-- =====================================================

CREATE TABLE privchat_users (
    user_id BIGSERIAL PRIMARY KEY,  -- uint64，数据库自增（从 100000000 开始）
    username VARCHAR(64) NOT NULL UNIQUE,
    phone VARCHAR(20) UNIQUE,
    email VARCHAR(255) UNIQUE,
    password_hash VARCHAR(255),  -- 内置账号系统密码哈希（bcrypt），允许 NULL（支持外部认证）
    display_name VARCHAR(128),
    avatar_url TEXT,
    user_type SMALLINT DEFAULT 0,  -- 0: Normal User, 1: System (保留), 2: Bot
    status SMALLINT DEFAULT 0,  -- 0: Active, 1: Inactive, 2: Suspended, 3: Deleted
    privacy_settings JSONB DEFAULT '{}',
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    updated_at BIGINT NOT NULL DEFAULT now_millis(),
    last_active_at BIGINT
);

-- 用户表索引
CREATE INDEX idx_privchat_users_username ON privchat_users (username);
CREATE INDEX idx_privchat_users_phone ON privchat_users (phone) WHERE phone IS NOT NULL;
CREATE INDEX idx_privchat_users_email ON privchat_users (email) WHERE email IS NOT NULL;
CREATE INDEX idx_privchat_users_type ON privchat_users (user_type);  -- 用户类型索引
CREATE INDEX idx_privchat_users_status ON privchat_users (status) WHERE status = 0;  -- 只索引活跃用户
CREATE INDEX idx_privchat_users_last_active ON privchat_users (last_active_at DESC) WHERE status = 0;

-- =====================================================
-- 1.1 用户设置表 (privchat_user_settings)
-- ENTITY_SYNC_V1 user_settings，表为主
-- =====================================================

CREATE TABLE IF NOT EXISTS privchat_user_settings (
    user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    setting_key VARCHAR(128) NOT NULL,
    value_json JSONB NOT NULL DEFAULT '{}',
    version BIGINT NOT NULL DEFAULT 1,
    updated_at BIGINT NOT NULL DEFAULT (extract(epoch from now()) * 1000)::BIGINT,
    PRIMARY KEY (user_id, setting_key)
);

CREATE INDEX idx_privchat_user_settings_user_version
    ON privchat_user_settings (user_id, version);

COMMENT ON TABLE privchat_user_settings IS '用户设置（ENTITY_SYNC_V1 user_settings），表为主';

-- =====================================================
-- 2. 设备表 (privchat_devices)
-- =====================================================

CREATE TABLE privchat_devices (
    device_id UUID NOT NULL,  -- 设备ID（客户端生成，支持同一设备登录多个账号）
    user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    device_type VARCHAR(32) NOT NULL,  -- ios, android, web, desktop
    device_name VARCHAR(128),
    device_model VARCHAR(128),
    os_version VARCHAR(64),
    app_version VARCHAR(32),
    last_active_at BIGINT,
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    updated_at BIGINT NOT NULL DEFAULT now_millis(),
    -- 会话管理字段（Token 版本控制）
    session_version BIGINT NOT NULL DEFAULT 1,  -- 会话世代，只在安全事件时递增
    session_state SMALLINT NOT NULL DEFAULT 0,  -- 0:ACTIVE, 1:KICKED, 2:FROZEN, 3:REVOKED, 4:PENDING_VERIFY
    kicked_at BIGINT,  -- 被踢时间（毫秒时间戳）
    kicked_by_device_id UUID,  -- 由哪个设备踢出
    kicked_reason VARCHAR(255),  -- 踢出原因或状态变更原因
    last_ip VARCHAR(45),  -- 最后活跃IP地址
    PRIMARY KEY (user_id, device_id)  -- 组合主键：支持同一设备登录多个账号
);

-- 设备表索引
CREATE INDEX idx_privchat_devices_user ON privchat_devices (user_id);
CREATE INDEX idx_privchat_devices_device ON privchat_devices (device_id);  -- 支持按设备ID查询（多账号场景）
CREATE INDEX idx_privchat_devices_user_active ON privchat_devices (user_id, last_active_at DESC);
-- 会话管理索引
CREATE INDEX idx_privchat_devices_session ON privchat_devices (user_id, session_state);
CREATE INDEX idx_privchat_devices_session_version ON privchat_devices (user_id, session_version);
CREATE INDEX idx_privchat_devices_kicked ON privchat_devices (kicked_at DESC) WHERE session_state = 1;

-- 设备表触发器（自动更新 updated_at）
CREATE TRIGGER update_privchat_devices_updated_at 
    BEFORE UPDATE ON privchat_devices
    FOR EACH ROW 
    EXECUTE FUNCTION update_updated_at_column();

-- =====================================================
-- 3. 好友关系表 (privchat_friendships)
-- =====================================================

CREATE TABLE privchat_friendships (
    user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    friend_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    status SMALLINT DEFAULT 0,  -- 0: Pending, 1: Accepted, 2: Blocked
    source VARCHAR(64),  -- 来源：search, qrcode, share_card, etc.
    source_id VARCHAR(256),  -- 来源ID，与 source 配套，可追溯
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    updated_at BIGINT NOT NULL DEFAULT now_millis(),
    PRIMARY KEY (user_id, friend_id),
    CHECK (user_id != friend_id)
);

-- 好友关系表索引
CREATE INDEX idx_privchat_friendships_user ON privchat_friendships (user_id, status);
CREATE INDEX idx_privchat_friendships_friend ON privchat_friendships (friend_id, status);
CREATE INDEX idx_privchat_friendships_accepted ON privchat_friendships (user_id, friend_id) 
    WHERE status = 1;  -- 只索引已接受的好友关系

-- =====================================================
-- 4. 黑名单表 (privchat_blacklist)
-- =====================================================

CREATE TABLE privchat_blacklist (
    user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    blocked_user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    PRIMARY KEY (user_id, blocked_user_id)
);

-- 黑名单表索引
CREATE INDEX idx_privchat_blacklist_user ON privchat_blacklist (user_id);

-- =====================================================
-- 5. 群组表 (privchat_groups)
-- =====================================================

CREATE TABLE privchat_groups (
    group_id BIGSERIAL PRIMARY KEY,  -- uint64，数据库自增
    name VARCHAR(128) NOT NULL,
    description TEXT,
    avatar_url TEXT,
    owner_id BIGINT NOT NULL REFERENCES privchat_users(user_id),
    settings JSONB DEFAULT '{}',  -- 群设置：公告、权限、审批等
    max_members INTEGER DEFAULT 500,
    member_count INTEGER DEFAULT 0,
    status SMALLINT DEFAULT 0,  -- 0: Active, 1: Dissolved
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    updated_at BIGINT NOT NULL DEFAULT now_millis()
);

-- 群组表索引
CREATE INDEX idx_privchat_groups_owner ON privchat_groups (owner_id);
CREATE INDEX idx_privchat_groups_status ON privchat_groups (status) WHERE status = 0;
CREATE INDEX idx_privchat_groups_settings_gin ON privchat_groups USING GIN (settings);  -- GIN 索引用于 JSONB 查询

-- =====================================================
-- 6. 群组成员表 (privchat_group_members)
-- =====================================================

CREATE TABLE privchat_group_members (
    group_id BIGINT NOT NULL REFERENCES privchat_groups(group_id) ON DELETE CASCADE,
    user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    role SMALLINT DEFAULT 2,  -- 0: Owner, 1: Admin, 2: Member
    nickname VARCHAR(128),  -- 群内昵称
    permissions JSONB DEFAULT '{}',  -- 个人权限设置
    mute_until BIGINT,  -- 禁言到期时间（毫秒时间戳）
    joined_at BIGINT NOT NULL DEFAULT now_millis(),
    left_at BIGINT,
    PRIMARY KEY (group_id, user_id)
);

-- 群组成员表索引
CREATE INDEX idx_privchat_group_members_group ON privchat_group_members (group_id) WHERE left_at IS NULL;
CREATE INDEX idx_privchat_group_members_user ON privchat_group_members (user_id) WHERE left_at IS NULL;
CREATE INDEX idx_privchat_group_members_role ON privchat_group_members (group_id, role) WHERE left_at IS NULL;

-- =====================================================
-- 7. 频道表 (privchat_channels)
-- =====================================================

CREATE TABLE privchat_channels (
    channel_id BIGSERIAL PRIMARY KEY,  -- uint64，数据库自增
    channel_type SMALLINT NOT NULL,  -- 0: Direct, 1: Group
    direct_user1_id BIGINT REFERENCES privchat_users(user_id),
    direct_user2_id BIGINT REFERENCES privchat_users(user_id),
    group_id BIGINT REFERENCES privchat_groups(group_id),
    last_message_id BIGINT,  -- 最后一条消息ID（Snowflake uint64）
    last_message_at BIGINT,
    message_count BIGINT DEFAULT 0,
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    updated_at BIGINT NOT NULL DEFAULT now_millis(),
    create_source VARCHAR(64),  -- 创建来源类型: search/phone/card_share/group/qrcode 等
    create_source_id VARCHAR(256),  -- 来源ID: 搜索会话id、群id、分享id、好友id 等
    CHECK (
        (channel_type = 0 AND direct_user1_id IS NOT NULL AND direct_user2_id IS NOT NULL AND group_id IS NULL) OR
        (channel_type = 1 AND group_id IS NOT NULL AND direct_user1_id IS NULL AND direct_user2_id IS NULL)
    )
);

-- 频道表索引
CREATE INDEX idx_privchat_channels_type ON privchat_channels (channel_type);
CREATE INDEX idx_privchat_channels_direct ON privchat_channels (direct_user1_id, direct_user2_id) 
    WHERE channel_type = 0;
CREATE INDEX idx_privchat_channels_group ON privchat_channels (group_id) WHERE channel_type = 1;
CREATE INDEX idx_privchat_channels_last_message ON privchat_channels (last_message_at DESC);

-- =====================================================
-- 8. 频道参与者表 (privchat_channel_participants)
-- =====================================================

CREATE TABLE privchat_channel_participants (
    channel_id BIGINT NOT NULL REFERENCES privchat_channels(channel_id) ON DELETE CASCADE,
    user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    role SMALLINT DEFAULT 2,  -- 0: Owner, 1: Admin, 2: Member
    nickname VARCHAR(128),  -- 会话内昵称
    permissions JSONB DEFAULT '{}',  -- 个人权限设置
    mute_until BIGINT,  -- 禁言到期时间（毫秒时间戳）
    joined_at BIGINT NOT NULL DEFAULT now_millis(),
    left_at BIGINT,
    PRIMARY KEY (channel_id, user_id)
);

-- 频道参与者表索引
CREATE INDEX idx_privchat_channel_participants_channel ON privchat_channel_participants (channel_id) WHERE left_at IS NULL;
CREATE INDEX idx_privchat_channel_participants_user ON privchat_channel_participants (user_id) WHERE left_at IS NULL;
CREATE INDEX idx_privchat_channel_participants_role ON privchat_channel_participants (channel_id, role) WHERE left_at IS NULL;

-- =====================================================
-- 9. 消息表 (privchat_messages) - 分区表
-- =====================================================

-- 主表（分区表）
CREATE TABLE privchat_messages (
    message_id BIGINT NOT NULL,  -- uint64，使用 Snowflake（由应用层生成）
    channel_id BIGINT NOT NULL REFERENCES privchat_channels(channel_id) ON DELETE CASCADE,
    sender_id BIGINT NOT NULL REFERENCES privchat_users(user_id),
    pts BIGINT NOT NULL,  -- pts 同步机制（必须保存）
    local_message_id BIGINT,  -- 客户端消息编号（用于去重，使用 Snowflake ID）
    message_type SMALLINT NOT NULL,
    content TEXT NOT NULL,
    metadata JSONB DEFAULT '{}',
    reply_to_message_id BIGINT,  -- 回复的消息ID（Snowflake uint64）
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    updated_at BIGINT NOT NULL DEFAULT now_millis(),
    deleted BOOLEAN DEFAULT false,
    deleted_at BIGINT,
    -- 撤回相关字段
    revoked BOOLEAN DEFAULT false,  -- 是否已撤回（快速查询）
    revoked_at BIGINT,  -- 撤回时间（毫秒时间戳）
    revoked_by BIGINT REFERENCES privchat_users(user_id),  -- 撤回者ID
    PRIMARY KEY (message_id, created_at)
) PARTITION BY RANGE (created_at);

-- 消息表索引（会自动应用到每个分区）
CREATE INDEX idx_privchat_messages_channel_time ON privchat_messages (channel_id, created_at DESC);
CREATE INDEX idx_privchat_messages_channel_id ON privchat_messages (channel_id, message_id DESC);  -- 利用Snowflake ID的时间有序性
CREATE INDEX idx_privchat_messages_sender ON privchat_messages (sender_id, created_at DESC);
CREATE INDEX idx_privchat_messages_pts ON privchat_messages (sender_id, pts);  -- pts 索引（用于恢复）
CREATE INDEX idx_privchat_messages_local_message_id ON privchat_messages (channel_id, local_message_id) 
    WHERE local_message_id IS NOT NULL;  -- 用于去重
CREATE INDEX idx_privchat_messages_reply ON privchat_messages (reply_to_message_id) 
    WHERE reply_to_message_id IS NOT NULL;
CREATE INDEX idx_privchat_messages_metadata_gin ON privchat_messages USING GIN (metadata);  -- JSONB GIN 索引
CREATE INDEX idx_privchat_messages_deleted ON privchat_messages (channel_id, created_at DESC) 
    WHERE deleted = false;  -- 只索引未删除的消息
CREATE INDEX idx_privchat_messages_revoked ON privchat_messages (channel_id, revoked_at) 
    WHERE revoked = true;  -- 索引已撤回的消息（使用 revoked 布尔字段）

-- 创建初始分区（2026年1月）
-- 2026-01-01 00:00:00 UTC = 1767196800000 毫秒
-- 2026-02-01 00:00:00 UTC = 1769875200000 毫秒
CREATE TABLE privchat_messages_2026_01 PARTITION OF privchat_messages
    FOR VALUES FROM (1767196800000) TO (1769875200000);  -- 2026-01-01 到 2026-02-01

-- 创建 2026年2月分区（提前创建，避免数据插入失败）
-- 2026-02-01 00:00:00 UTC = 1769875200000 毫秒
-- 2026-03-01 00:00:00 UTC = 1772294400000 毫秒
CREATE TABLE privchat_messages_2026_02 PARTITION OF privchat_messages
    FOR VALUES FROM (1769875200000) TO (1772294400000);  -- 2026-02-01 到 2026-03-01

-- =====================================================
-- 10. 已读回执表 (privchat_read_receipts)
-- =====================================================

CREATE TABLE privchat_read_receipts (
    message_id BIGINT NOT NULL,  -- Snowflake uint64
    user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    channel_id BIGINT NOT NULL REFERENCES privchat_channels(channel_id) ON DELETE CASCADE,
    read_at BIGINT NOT NULL DEFAULT now_millis(),
    PRIMARY KEY (message_id, user_id)
);

-- 已读回执表索引
CREATE INDEX idx_privchat_read_receipts_user_channel ON privchat_read_receipts (user_id, channel_id);
CREATE INDEX idx_privchat_read_receipts_channel ON privchat_read_receipts (channel_id);

-- =====================================================
-- 11. 用户频道列表表 (privchat_user_channels)
-- =====================================================

CREATE TABLE privchat_user_channels (
    user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    channel_id BIGINT NOT NULL REFERENCES privchat_channels(channel_id) ON DELETE CASCADE,
    last_read_message_id BIGINT,  -- Snowflake uint64
    last_read_at BIGINT,
    unread_count INTEGER DEFAULT 0,
    is_pinned BOOLEAN DEFAULT false,
    is_muted BOOLEAN DEFAULT false,
    updated_at BIGINT NOT NULL DEFAULT now_millis(),
    PRIMARY KEY (user_id, channel_id)
);

-- 用户频道列表表索引
CREATE INDEX idx_privchat_user_channels_user_updated ON privchat_user_channels (user_id, updated_at DESC);
CREATE INDEX idx_privchat_user_channels_user_pinned ON privchat_user_channels (user_id, is_pinned DESC, updated_at DESC);
CREATE INDEX idx_privchat_user_channels_unread ON privchat_user_channels (user_id, unread_count) 
    WHERE unread_count > 0;

-- =====================================================
-- 12. 设备同步状态表 (privchat_device_sync_state)
-- =====================================================

CREATE TABLE privchat_device_sync_state (
    user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    device_id UUID NOT NULL,
    channel_id BIGINT NOT NULL REFERENCES privchat_channels(channel_id) ON DELETE CASCADE,
    local_pts BIGINT DEFAULT 0,
    server_pts BIGINT DEFAULT 0,
    last_sync_at BIGINT NOT NULL DEFAULT now_millis(),
    PRIMARY KEY (user_id, device_id, channel_id),
    FOREIGN KEY (user_id, device_id) REFERENCES privchat_devices(user_id, device_id) ON DELETE CASCADE
);

-- 设备同步状态表索引
CREATE INDEX idx_privchat_sync_state_user_device ON privchat_device_sync_state (user_id, device_id);
CREATE INDEX idx_privchat_sync_state_channel ON privchat_device_sync_state (channel_id);

-- =====================================================
-- 13. 用户最后上线时间表 (privchat_user_last_seen)
-- =====================================================
-- 用于持久化用户的最后活跃时间，支持服务重启后恢复数据
-- 保留30天数据，定期清理过期记录

CREATE TABLE privchat_user_last_seen (
    user_id BIGINT PRIMARY KEY REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    last_seen_at BIGINT NOT NULL  -- Unix 时间戳（秒），同时表示最后更新时间
);

-- 用户最后上线时间表索引
CREATE INDEX idx_privchat_user_last_seen_time ON privchat_user_last_seen (last_seen_at);  -- 用于清理过期数据

-- =====================================================
-- 表注释
-- =====================================================

COMMENT ON TABLE privchat_users IS '用户表：存储用户基本信息（user_id: BIGSERIAL）';
COMMENT ON TABLE privchat_devices IS '设备表：存储用户设备信息（device_id: UUID）';
COMMENT ON TABLE privchat_friendships IS '好友关系表：存储用户之间的好友关系';
COMMENT ON COLUMN privchat_friendships.source_id IS '来源ID，与 source 配套，可追溯';
COMMENT ON TABLE privchat_blacklist IS '黑名单表：存储用户黑名单';
COMMENT ON TABLE privchat_groups IS '群组表：存储群组基本信息（group_id: BIGSERIAL）';
COMMENT ON TABLE privchat_group_members IS '群组成员表：存储群组成员信息';
COMMENT ON TABLE privchat_channels IS '频道表：存储频道基本信息（channel_id: BIGSERIAL）';
COMMENT ON COLUMN privchat_channels.create_source IS '创建来源类型: search/phone/card_share/group/qrcode 等';
COMMENT ON COLUMN privchat_channels.create_source_id IS '来源ID: 搜索会话id、群id、分享id、好友id 等';
COMMENT ON TABLE privchat_channel_participants IS '频道参与者表：存储频道参与者信息';
COMMENT ON TABLE privchat_messages IS '消息表：存储所有消息（message_id: Snowflake uint64，分区表，按月分区）';
COMMENT ON TABLE privchat_read_receipts IS '已读回执表：存储消息已读状态';
COMMENT ON TABLE privchat_user_channels IS '用户频道列表表：存储用户的频道列表视图';
COMMENT ON TABLE privchat_device_sync_state IS '设备同步状态表：存储设备的 pts 同步状态';
COMMENT ON TABLE privchat_user_last_seen IS '用户最后上线时间表：存储用户最后活跃时间，保留30天数据';

-- =====================================================
-- 14. 频道 pts 表 (privchat_channel_pts)
-- Phase 8: pts-based 同步机制
-- =====================================================

CREATE TABLE privchat_channel_pts (
    channel_id BIGINT NOT NULL,
    channel_type SMALLINT NOT NULL,  -- 1=私聊，2=群聊
    current_pts BIGINT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    updated_at BIGINT NOT NULL DEFAULT now_millis(),
    PRIMARY KEY (channel_id, channel_type)
);

-- 频道 pts 表索引
CREATE INDEX idx_privchat_channel_pts_updated ON privchat_channel_pts(updated_at);

-- 表注释
COMMENT ON TABLE privchat_channel_pts IS '频道 pts 表：存储每个频道的当前 pts（per-channel 单调递增）';

-- =====================================================
-- 15. Commit Log 表 (privchat_commit_log)
-- Phase 8: 服务器权威 Commit Log
-- =====================================================

CREATE TABLE privchat_commit_log (
    id BIGSERIAL PRIMARY KEY,
    pts BIGINT NOT NULL,  -- per-channel 单调递增序号
    server_msg_id BIGINT NOT NULL,  -- 服务器消息 ID（全局唯一，Snowflake）
    local_message_id BIGINT,  -- 客户端消息号（用于关联和幂等，Snowflake）
    channel_id BIGINT NOT NULL,
    channel_type SMALLINT NOT NULL,  -- 1=私聊，2=群聊
    message_type VARCHAR(50) NOT NULL,  -- text/image/video/revoke/delete/edit/reaction 等
    content JSONB NOT NULL,  -- 消息内容（JSON）
    server_timestamp BIGINT NOT NULL,  -- 服务器时间戳（毫秒）
    sender_id BIGINT NOT NULL REFERENCES privchat_users(user_id),
    sender_username VARCHAR(100),  -- 发送者用户名（冗余，加速查询）
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    
    -- 唯一索引（防止 pts 重复）
    UNIQUE (channel_id, channel_type, pts)
);

-- Commit Log 表索引
CREATE INDEX idx_privchat_commit_log_channel_pts ON privchat_commit_log (channel_id, channel_type, pts);
CREATE INDEX idx_privchat_commit_log_timestamp ON privchat_commit_log (server_timestamp);
CREATE INDEX idx_privchat_commit_log_local_message_id ON privchat_commit_log (local_message_id) 
    WHERE local_message_id IS NOT NULL;

-- 表注释
COMMENT ON TABLE privchat_commit_log IS 'Commit Log：权威事实，按 pts 严格递增';

-- =====================================================
-- 16. 客户端消息号注册表 (privchat_client_msg_registry)
-- Phase 8: 幂等性保证
-- =====================================================

CREATE TABLE privchat_client_msg_registry (
    local_message_id BIGINT PRIMARY KEY,  -- 客户端消息号（Snowflake u64）
    server_msg_id BIGINT NOT NULL,  -- 服务器消息 ID
    pts BIGINT NOT NULL,  -- 分配的 pts
    channel_id BIGINT NOT NULL,
    channel_type SMALLINT NOT NULL,
    sender_id BIGINT NOT NULL REFERENCES privchat_users(user_id),
    decision VARCHAR(20) NOT NULL DEFAULT 'accepted',  -- accepted/transformed/rejected
    created_at BIGINT NOT NULL DEFAULT now_millis()
);

-- 客户端消息号注册表索引
CREATE INDEX idx_privchat_client_msg_reg_created ON privchat_client_msg_registry (created_at);
CREATE INDEX idx_privchat_client_msg_reg_server_id ON privchat_client_msg_registry (server_msg_id);

-- 表注释
COMMENT ON TABLE privchat_client_msg_registry IS '客户端消息号注册表：用于幂等性检查';

-- =====================================================
-- 17. 离线消息队列表 (privchat_offline_message_queue)
-- Phase 8: 基于 pts 的轻量级离线消息队列（可选）
-- =====================================================

CREATE TABLE privchat_offline_message_queue (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    channel_id BIGINT NOT NULL,
    channel_type SMALLINT NOT NULL,
    pts BIGINT NOT NULL,  -- Commit 的 pts
    server_msg_id BIGINT NOT NULL,  -- 服务器消息 ID
    delivered SMALLINT NOT NULL DEFAULT 0,  -- 0=未投递，1=已投递
    delivered_at BIGINT,  -- 投递时间
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    expires_at BIGINT NOT NULL  -- 过期时间（7天后）
);

-- 离线消息队列表索引
CREATE INDEX idx_privchat_offline_queue_user ON privchat_offline_message_queue (user_id, delivered, created_at);
CREATE INDEX idx_privchat_offline_queue_expires ON privchat_offline_message_queue (expires_at);

-- 表注释
COMMENT ON TABLE privchat_offline_message_queue IS '离线消息队列：基于 pts 的轻量级队列';

-- =====================================================
-- 触发器设置
-- =====================================================

-- 为新表添加 updated_at 自动更新触发器
CREATE TRIGGER update_privchat_channel_pts_updated_at BEFORE UPDATE ON privchat_channel_pts
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- =====================================================
-- Phase 8 说明
-- =====================================================

-- pts 职责：
-- - pts 是 per-channel 单调递增（不是 per-user）
-- - pts 由服务器分配，客户端不能自己生成
-- - pts 用于权威顺序

-- local_message_id 职责：
-- - 客户端生成（Snowflake u64）
-- - 用于幂等（防止重复提交）
-- - 用于关联（匹配客户端和服务器消息）

-- 查询示例：
-- 
-- 1. 获取频道当前 pts
-- SELECT current_pts FROM privchat_channel_pts 
-- WHERE channel_id = ? AND channel_type = ?;
-- 
-- 2. 获取差异（pts > last_pts）
-- SELECT * FROM privchat_commit_log 
-- WHERE channel_id = ? AND channel_type = ? AND pts > ?
-- ORDER BY pts ASC
-- LIMIT 100;
-- 
-- 3. 检查 local_message_id 是否重复
-- SELECT server_msg_id, pts FROM privchat_client_msg_registry 
-- WHERE local_message_id = ?;

-- =====================================================
-- 用户 ID 序列设置
-- =====================================================
-- 
-- 用户 ID 区间划分：
-- - 1 ~ 99: 系统功能用户（系统消息、文件助手等，不在数据库中）
-- - 100,000,000+: 普通用户 + 机器人（用 user_type 字段区分）
-- 
-- 设置序列起始值为 100000000，保留 1~99 给系统功能用户

DO $$
DECLARE
    user_count INTEGER;
BEGIN
    -- 检查用户表是否为空
    SELECT COUNT(*) INTO user_count FROM privchat_users;
    
    IF user_count = 0 THEN
        -- 表为空，设置序列起始值
        PERFORM setval('privchat_users_user_id_seq', 100000000, false);
        RAISE NOTICE '✅ 用户 ID 序列起始值已设置为 100000000';
    ELSE
        -- 表不为空，警告
        RAISE WARNING '⚠️  用户表不为空（现有用户数: %），跳过序列起始值设置', user_count;
    END IF;
END $$;

-- 添加序列注释
COMMENT ON SEQUENCE privchat_users_user_id_seq IS '用户 ID 序列：从 100000000 开始，1~99 保留给系统功能用户';

-- =====================================================
-- 18. 登录日志表 (privchat_login_logs)
-- 功能: 记录用户 token 首次认证的登录行为
-- 设计原则:
--   1. 只记录 token 首次使用（首次 AuthorizationRequest）
--   2. 重复使用同一 token 不记录（避免日志爆炸）
--   3. 强关联 device_id（可追溯设备的所有登录历史）
--   4. 支持地理位置和风险评分
--   5. 支持发送登录通知（系统账户1）
-- =====================================================

CREATE TABLE privchat_login_logs (
    log_id BIGSERIAL PRIMARY KEY,
    
    -- 关联信息
    user_id BIGINT NOT NULL REFERENCES privchat_users(user_id) ON DELETE CASCADE,
    device_id UUID NOT NULL,
    
    -- Token 信息
    token_jti VARCHAR(64) NOT NULL,  -- JWT ID（用于关联和撤销检测）
    token_created_at BIGINT NOT NULL,  -- token 创建时间（毫秒时间戳，从 JWT iat 提取）
    token_first_used_at BIGINT NOT NULL DEFAULT now_millis(),  -- token 首次认证时间（真正的登录时间）
    
    -- 设备信息（冗余存储，便于追溯）
    device_type VARCHAR(32) NOT NULL,  -- ios/android/web/desktop/macos/windows
    device_name VARCHAR(128),  -- 设备名称（如 "iPhone 14 Pro"）
    device_model VARCHAR(128),  -- 设备型号
    os_version VARCHAR(64),  -- 操作系统版本
    app_id VARCHAR(64) NOT NULL,  -- 应用ID
    app_version VARCHAR(32),  -- 应用版本
    
    -- 网络信息
    ip_address VARCHAR(45) NOT NULL,  -- IPv4/IPv6
    user_agent TEXT,  -- 浏览器/客户端 User-Agent
    
    -- 登录方式
    login_method VARCHAR(32) NOT NULL,  -- register/login/token_refresh/oauth/sso
    auth_source VARCHAR(64),  -- 认证源（如 "privchat-internal", "oauth-google", "sso-enterprise"）
    
    -- 地理位置信息（可选，通过 IP 解析）
    country VARCHAR(64),
    country_code VARCHAR(3),  -- ISO 3166-1 alpha-2/3
    region VARCHAR(128),
    city VARCHAR(128),
    latitude DECIMAL(10, 8),
    longitude DECIMAL(11, 8),
    timezone VARCHAR(64),  -- 时区
    isp VARCHAR(128),  -- ISP/运营商
    
    -- 安全检测
    status SMALLINT NOT NULL DEFAULT 0,  -- 0: Success, 1: Suspicious, 2: Blocked
    risk_score SMALLINT NOT NULL DEFAULT 0,  -- 风险评分 0-100
    risk_factors JSONB DEFAULT '[]',  -- 风险因素数组（如 ["new_location", "unusual_time"]）
    is_new_device BOOLEAN NOT NULL DEFAULT false,  -- 是否为新设备首次登录
    is_new_location BOOLEAN NOT NULL DEFAULT false,  -- 是否为新地理位置
    
    -- 通知状态
    notification_sent BOOLEAN NOT NULL DEFAULT false,  -- 是否已发送登录通知
    notification_sent_at BIGINT,  -- 通知发送时间
    notification_method VARCHAR(32),  -- 通知方式（system_message/email/push）
    
    -- 额外元数据
    metadata JSONB DEFAULT '{}',  -- 额外信息（如检测到的异常、备注等）
    
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    
    -- 组合外键：引用设备表（支持同一设备登录多个账号）
    FOREIGN KEY (user_id, device_id) REFERENCES privchat_devices(user_id, device_id) ON DELETE CASCADE
);

-- 登录日志表索引
CREATE INDEX idx_privchat_login_logs_user_time ON privchat_login_logs (user_id, token_first_used_at DESC);
CREATE INDEX idx_privchat_login_logs_device ON privchat_login_logs (device_id, token_first_used_at DESC);
CREATE INDEX idx_privchat_login_logs_token_jti ON privchat_login_logs (token_jti);
CREATE INDEX idx_privchat_login_logs_ip ON privchat_login_logs (ip_address);
CREATE INDEX idx_privchat_login_logs_created ON privchat_login_logs (created_at DESC);
CREATE INDEX idx_privchat_login_logs_status ON privchat_login_logs (user_id, status);
CREATE INDEX idx_privchat_login_logs_notification ON privchat_login_logs (user_id, notification_sent) WHERE notification_sent = false;
CREATE INDEX idx_privchat_login_logs_risk ON privchat_login_logs (user_id, risk_score DESC) WHERE risk_score > 50;
CREATE INDEX idx_privchat_login_logs_new_device ON privchat_login_logs (user_id, created_at DESC) WHERE is_new_device = true;
CREATE INDEX idx_privchat_login_logs_risk_factors ON privchat_login_logs USING GIN (risk_factors);

-- 登录日志表注释
COMMENT ON TABLE privchat_login_logs IS '登录日志表：记录用户 token 首次认证的登录行为，关联到具体设备';
COMMENT ON COLUMN privchat_login_logs.device_id IS '设备ID（强关联），可追溯该设备的所有登录历史';
COMMENT ON COLUMN privchat_login_logs.token_jti IS 'JWT ID，可用于关联 token 撤销记录和去重检测';
COMMENT ON COLUMN privchat_login_logs.token_first_used_at IS 'Token 首次认证时间（真正的登录时间）';
COMMENT ON COLUMN privchat_login_logs.is_new_device IS '该设备是否为首次登录（device_id 首次出现）';
COMMENT ON COLUMN privchat_login_logs.is_new_location IS '该用户是否从新地理位置登录';
COMMENT ON COLUMN privchat_login_logs.risk_score IS '风险评分 0-100，根据多种因素计算';
COMMENT ON COLUMN privchat_login_logs.risk_factors IS '风险因素数组，如 ["new_location", "unusual_time", "proxy_or_vpn"]';

-- =====================================================
-- 设备表字段注释补充
-- =====================================================

COMMENT ON COLUMN privchat_devices.session_version IS '会话世代，只在安全事件时递增（登出、改密、被踢等）';
COMMENT ON COLUMN privchat_devices.session_state IS '会话状态：0=活跃,1=被踢,2=冻结,3=撤销,4=待验证';
COMMENT ON COLUMN privchat_devices.kicked_at IS '被踢时间（毫秒时间戳）';
COMMENT ON COLUMN privchat_devices.kicked_by_device_id IS '由哪个设备踢出';
COMMENT ON COLUMN privchat_devices.kicked_reason IS '踢出原因或状态变更原因';
COMMENT ON COLUMN privchat_devices.last_ip IS '最后活跃IP地址';

-- =====================================================
-- 19. 用户设备推送表 (privchat_user_devices)
-- Phase 3.5: 推送系统设备管理（包含推送状态字段）
-- =====================================================

CREATE TABLE IF NOT EXISTS privchat_user_devices (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL,
    device_id VARCHAR(128) NOT NULL,
    platform VARCHAR(32) NOT NULL,  -- ios / android / desktop
    vendor VARCHAR(32) NOT NULL,     -- apns / fcm（MVP 只支持这两个）
    push_token TEXT,                 -- 推送令牌
    apns_armed BOOLEAN DEFAULT false,  -- ✨ Phase 3.5: 是否需要推送（客户端声明能力）
    connected BOOLEAN DEFAULT false,  -- ✨ Phase 3.5: 是否存在可用长连接（QUIC/WebSocket/TCP，事实状态）
    last_send_ts BIGINT,              -- ✨ Phase 3.5: 最近一次发送成功时间
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW(),
    UNIQUE(user_id, device_id)
);

-- 用户设备推送表索引
CREATE INDEX IF NOT EXISTS idx_privchat_user_devices_user_id ON privchat_user_devices(user_id);
CREATE INDEX IF NOT EXISTS idx_privchat_user_devices_connected 
ON privchat_user_devices(connected) WHERE connected = true;  -- ✨ Phase 3.5
CREATE INDEX IF NOT EXISTS idx_privchat_user_devices_apns_armed 
ON privchat_user_devices(apns_armed) WHERE apns_armed = true;  -- ✨ Phase 3.5

-- 用户设备推送表注释
COMMENT ON TABLE privchat_user_devices IS '用户设备推送信息表（Phase 3.5: 包含推送状态字段）';
COMMENT ON COLUMN privchat_user_devices.user_id IS '用户ID';
COMMENT ON COLUMN privchat_user_devices.device_id IS '设备ID';
COMMENT ON COLUMN privchat_user_devices.platform IS '平台：ios / android / desktop';
COMMENT ON COLUMN privchat_user_devices.vendor IS '推送平台：apns / fcm';
COMMENT ON COLUMN privchat_user_devices.push_token IS '推送令牌';
COMMENT ON COLUMN privchat_user_devices.apns_armed IS '是否需要推送（客户端声明能力）';  -- ✨ Phase 3.5
COMMENT ON COLUMN privchat_user_devices.connected IS '是否存在可用长连接（QUIC/WebSocket/TCP，事实状态）';  -- ✨ Phase 3.5
COMMENT ON COLUMN privchat_user_devices.last_send_ts IS '最近一次发送成功时间';  -- ✨ Phase 3.5

-- =====================================================
-- 20. 文件上传记录表 (privchat_file_uploads)
-- 上传服务持久化，有据可查、清理不依赖缓存
-- =====================================================

CREATE SEQUENCE IF NOT EXISTS privchat_file_uploads_id_seq;

CREATE TABLE IF NOT EXISTS privchat_file_uploads (
    file_id BIGINT PRIMARY KEY DEFAULT nextval('privchat_file_uploads_id_seq'),
    original_filename VARCHAR(512) NOT NULL,
    file_size BIGINT NOT NULL,
    file_type VARCHAR(32) NOT NULL,
    mime_type VARCHAR(128) NOT NULL,
    file_path TEXT NOT NULL,
    storage_source_id INT NOT NULL DEFAULT 0,
    uploader_id BIGINT NOT NULL,
    uploader_ip VARCHAR(45),
    uploaded_at BIGINT NOT NULL DEFAULT now_millis(),
    width INT,
    height INT,
    file_hash VARCHAR(128),
    business_type VARCHAR(64),
    business_id VARCHAR(128)
);

CREATE INDEX idx_privchat_file_uploads_uploader_id ON privchat_file_uploads(uploader_id);
CREATE INDEX idx_privchat_file_uploads_uploaded_at ON privchat_file_uploads(uploaded_at);
CREATE INDEX idx_privchat_file_uploads_business ON privchat_file_uploads(business_type, business_id);

COMMENT ON TABLE privchat_file_uploads IS '文件上传记录（持久化，清理与审计不依赖内存缓存）';
COMMENT ON COLUMN privchat_file_uploads.file_id IS '文件ID（BIGSERIAL 自增，u64）';
COMMENT ON COLUMN privchat_file_uploads.file_path IS '存储路径，如 public/chat/message/202601/{file_hash}，与 storage_source_id 配合定位文件';
COMMENT ON COLUMN privchat_file_uploads.storage_source_id IS '存储源：0=本地，1=S3，2=阿里云 OSS，3=腾讯云 COS 等';
COMMENT ON COLUMN privchat_file_uploads.uploader_id IS '上传者用户ID';
COMMENT ON COLUMN privchat_file_uploads.uploader_ip IS '上传时客户端 IP（IPv4/IPv6，VARCHAR(45)），便于审计与安全';
COMMENT ON COLUMN privchat_file_uploads.uploaded_at IS '上传时间（毫秒时间戳，u64）';
COMMENT ON COLUMN privchat_file_uploads.business_type IS '业务类型（如 message/avatar/group_avatar），便于按业务清理';
COMMENT ON COLUMN privchat_file_uploads.business_id IS '业务具体ID（字符串，兼容各类业务如 message_id/uuid 等），便于随业务数据删除时清理';

-- =====================================================
-- 完成
-- =====================================================
