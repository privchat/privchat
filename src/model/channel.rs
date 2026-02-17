use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use sqlx::Type;

// ============================================================================
// 类型别名
// ============================================================================

/// 频道ID类型
pub type ChannelId = u64;
/// 用户ID类型
pub type UserId = u64;
/// 群组ID类型
pub type GroupId = u64;
/// 消息ID类型
pub type MessageId = u64;
/// 设备ID类型（保留UUID字符串）
pub type DeviceId = String;

// ============================================================================
// 频道类型和状态
// ============================================================================

/// 频道类型（数据库兼容）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[sqlx(type_name = "smallint")]
pub enum ChannelType {
    /// 私聊（1v1）
    #[default]
    Direct = 0,
    /// 群聊
    Group = 1,
    /// 系统频道（通知等）
    System = 2,
}

impl ChannelType {
    /// 从 i16 转换
    pub fn from_i16(value: i16) -> Self {
        match value {
            0 => ChannelType::Direct,
            1 => ChannelType::Group,
            2 => ChannelType::System,
            _ => ChannelType::Direct,
        }
    }

    /// 转换为 i16
    pub fn to_i16(self) -> i16 {
        self as i16
    }
}

/// 频道类型 - 统一抽象所有消息场景（业务层使用）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelKind {
    /// 私聊 - 两人对话
    PrivateChat,
    /// 群聊 - 多人聊天室
    GroupChat,
    /// 单向广播 - 订阅号/频道（公众号模式）
    Broadcast,
}

impl From<ChannelType> for ChannelKind {
    fn from(ty: ChannelType) -> Self {
        match ty {
            ChannelType::Direct => ChannelKind::PrivateChat,
            ChannelType::Group => ChannelKind::GroupChat,
            ChannelType::System => ChannelKind::Broadcast,
        }
    }
}

impl From<ChannelKind> for ChannelType {
    fn from(kind: ChannelKind) -> Self {
        match kind {
            ChannelKind::PrivateChat => ChannelType::Direct,
            ChannelKind::GroupChat => ChannelType::Group,
            ChannelKind::Broadcast => ChannelType::System,
        }
    }
}

/// 会话状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[sqlx(type_name = "smallint")]
pub enum ChannelStatus {
    /// 活跃
    #[default]
    Active = 0,
    /// 已归档
    Archived = 1,
    /// 已删除
    Deleted = 2,
    /// 被封禁
    Banned = 3,
}

impl ChannelStatus {
    /// 从 i16 转换
    pub fn from_i16(value: i16) -> Self {
        match value {
            0 => ChannelStatus::Active,
            1 => ChannelStatus::Archived,
            2 => ChannelStatus::Deleted,
            3 => ChannelStatus::Banned,
            _ => ChannelStatus::Active,
        }
    }

    /// 转换为 i16
    pub fn to_i16(self) -> i16 {
        self as i16
    }
}

// ============================================================================
// 成员角色和权限
// ============================================================================

/// 会话成员角色（简化版，参考微信）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[repr(u8)]
pub enum MemberRole {
    /// 群主（仅群聊）
    Owner = 0,
    /// 管理员
    Admin = 1,
    /// 普通成员
    #[default]
    Member = 2,
}

impl MemberRole {
    /// 从 i16 转换
    pub fn from_i16(value: i16) -> Self {
        match value {
            0 => MemberRole::Owner,
            1 => MemberRole::Admin,
            2 => MemberRole::Member,
            _ => MemberRole::Member,
        }
    }

    /// 转换为 i16
    pub fn to_i16(self) -> i16 {
        self as i16
    }
}

/// 会话成员权限（完整版，预留未来功能）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberPermissions {
    // === 基础权限 ===
    /// 是否可以发送消息
    pub can_send_message: bool,
    /// 是否可以邀请成员
    pub can_invite: bool,
    /// 是否可以移除成员
    pub can_remove_member: bool,
    /// 是否可以修改会话信息（名称、头像、描述）
    pub can_edit_info: bool,
    /// 是否可以管理权限（设置管理员、修改权限）
    pub can_manage_permissions: bool,

    // === 消息管理权限（预留，暂不实现） ===
    /// 撤回任意成员的消息
    pub can_revoke_any_message: bool,
    /// 置顶消息
    pub can_pin_message: bool,

    // === 特殊权限（预留，暂不实现） ===
    /// @所有人
    pub can_at_all: bool,
    /// 修改群设置
    pub can_edit_settings: bool,
}

impl Default for MemberPermissions {
    fn default() -> Self {
        Self {
            can_send_message: true,
            can_invite: false,
            can_remove_member: false,
            can_edit_info: false,
            can_manage_permissions: false,
            can_revoke_any_message: false,
            can_pin_message: false,
            can_at_all: false,
            can_edit_settings: false,
        }
    }
}

impl MemberPermissions {
    /// 群主权限（全部权限）
    pub fn owner() -> Self {
        Self {
            can_send_message: true,
            can_invite: true,
            can_remove_member: true,
            can_edit_info: true,
            can_manage_permissions: true,
            can_revoke_any_message: true,
            can_pin_message: true,
            can_at_all: true,
            can_edit_settings: true,
        }
    }

    /// 管理员权限（除管理权限和群设置外的所有权限）
    pub fn admin() -> Self {
        Self {
            can_send_message: true,
            can_invite: true,
            can_remove_member: true,
            can_edit_info: true,
            can_manage_permissions: false, // 不能设置管理员
            can_revoke_any_message: true,
            can_pin_message: true,
            can_at_all: true,
            can_edit_settings: false, // 不能修改群设置
        }
    }

    /// 普通成员权限（仅基础权限）
    pub fn member() -> Self {
        Self {
            can_send_message: true,
            can_invite: false,
            can_remove_member: false,
            can_edit_info: false,
            can_manage_permissions: false,
            can_revoke_any_message: false, // 只能撤回自己的
            can_pin_message: false,
            can_at_all: false,
            can_edit_settings: false,
        }
    }

    /// 根据角色获取权限
    pub fn from_role(role: MemberRole) -> Self {
        match role {
            MemberRole::Owner => Self::owner(),
            MemberRole::Admin => Self::admin(),
            MemberRole::Member => Self::member(),
        }
    }
}

// ============================================================================
// 频道成员
// ============================================================================

/// 会话成员（业务层使用的内存结构）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMember {
    /// 用户ID
    pub user_id: u64,
    /// 用户昵称（会话内显示名）
    pub display_name: Option<String>,
    /// 成员角色
    pub role: MemberRole,
    /// 成员权限
    pub permissions: MemberPermissions,
    /// 加入时间
    pub joined_at: DateTime<Utc>,
    /// 最后活跃时间
    pub last_active_at: DateTime<Utc>,
    /// 是否静音
    pub is_muted: bool,
    /// 是否已读最新消息（兼容单条已读，可选）
    pub last_read_message_id: Option<u64>,
    /// 最后已读 pts（区间语义，主模型）：已读 pts <= last_read_pts 的所有消息
    pub last_read_pts: u64,
}

impl ChannelMember {
    /// 创建新成员
    pub fn new(user_id: u64, role: MemberRole) -> Self {
        let permissions = match role {
            MemberRole::Owner => MemberPermissions::owner(),
            MemberRole::Admin => MemberPermissions::admin(),
            MemberRole::Member => MemberPermissions::member(),
        };

        Self {
            user_id,
            display_name: None,
            role,
            permissions,
            joined_at: Utc::now(),
            last_active_at: Utc::now(),
            is_muted: false,
            last_read_message_id: None,
            last_read_pts: 0,
        }
    }

    /// 更新最后活跃时间
    pub fn update_last_active(&mut self) {
        self.last_active_at = Utc::now();
    }

    /// 更新已读消息（单条，兼容旧 RPC）
    pub fn mark_read(&mut self, message_id: u64) {
        self.last_read_message_id = Some(message_id);
        self.update_last_active();
    }

    /// 按 pts 推进已读（正确模型，O(1)）：last_read_pts = max(last_read_pts, read_pts)，天然幂等、单调
    pub fn mark_read_pts(&mut self, read_pts: u64) {
        self.last_read_pts = self.last_read_pts.max(read_pts);
        self.update_last_active();
    }
}

/// 会话参与者（对应 privchat_channel_participants 表）
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelParticipant {
    /// 会话ID
    pub channel_id: u64,
    /// 用户ID
    pub user_id: u64,
    /// 成员角色
    pub role: MemberRole,
    /// 群内昵称
    pub nickname: Option<String>,
    /// 个人权限设置（JSONB）
    pub permissions: MemberPermissions,
    /// 禁言到期时间（数据库存储为 BIGINT 毫秒时间戳）
    pub mute_until: Option<DateTime<Utc>>,
    /// 加入时间（数据库存储为 BIGINT 毫秒时间戳）
    pub joined_at: DateTime<Utc>,
    /// 离开时间（数据库存储为 BIGINT 毫秒时间戳）
    pub left_at: Option<DateTime<Utc>>,
}

impl ChannelParticipant {
    /// 从数据库行创建（处理时间戳和类型转换）
    pub fn from_db_row(
        channel_id: i64, // PostgreSQL BIGINT
        user_id: i64,    // PostgreSQL BIGINT
        role: i16,
        nickname: Option<String>,
        permissions: serde_json::Value,
        mute_until: Option<i64>, // 毫秒时间戳
        joined_at: i64,          // 毫秒时间戳
        left_at: Option<i64>,    // 毫秒时间戳
    ) -> Self {
        Self {
            channel_id: channel_id as u64,
            user_id: user_id as u64,
            role: MemberRole::from_i16(role),
            nickname,
            permissions: serde_json::from_value(permissions)
                .unwrap_or_else(|_| MemberPermissions::default()),
            mute_until: mute_until.and_then(|ts| DateTime::from_timestamp_millis(ts)),
            joined_at: DateTime::from_timestamp_millis(joined_at).unwrap_or_else(|| Utc::now()),
            left_at: left_at.and_then(|ts| DateTime::from_timestamp_millis(ts)),
        }
    }

    /// 转换为数据库插入值
    pub fn to_db_values(
        &self,
    ) -> (
        i64,
        i64,
        i16,
        Option<String>,
        serde_json::Value,
        Option<i64>,
        i64,
        Option<i64>,
    ) {
        (
            self.channel_id as i64,
            self.user_id as i64,
            self.role as i16,
            self.nickname.clone(),
            serde_json::to_value(&self.permissions)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new())),
            self.mute_until.map(|dt| dt.timestamp_millis()),
            self.joined_at.timestamp_millis(),
            self.left_at.map(|dt| dt.timestamp_millis()),
        )
    }

    /// 转换为业务层的 ChannelMember
    pub fn to_channel_member(&self) -> ChannelMember {
        ChannelMember {
            user_id: self.user_id, // u64，不需要转换
            display_name: self.nickname.clone(),
            role: self.role,
            permissions: self.permissions.clone(),
            joined_at: self.joined_at,
            last_active_at: self.joined_at, // 默认使用加入时间
            is_muted: self.mute_until.map_or(false, |dt| dt > Utc::now()),
            last_read_message_id: None, // 需要从其他表查询
            last_read_pts: 0,
        }
    }
}

// ============================================================================
// 频道元数据和设置
// ============================================================================

/// 会话元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMetadata {
    /// 会话名称
    pub name: Option<String>,
    /// 会话描述
    pub description: Option<String>,
    /// 会话头像URL
    pub avatar_url: Option<String>,
    /// 自定义属性
    pub custom_properties: HashMap<String, String>,
    /// 是否允许邀请
    pub allow_invite: bool,
    /// 最大成员数
    pub max_members: Option<usize>,
    /// 是否公开（可搜索）
    pub is_public: bool,
    /// 频道公告（可选）
    pub announcement: Option<String>,
    /// 标签
    pub tags: Vec<String>,
}

impl Default for ChannelMetadata {
    fn default() -> Self {
        Self {
            name: None,
            description: None,
            avatar_url: None,
            custom_properties: HashMap::new(),
            allow_invite: true,
            max_members: None,
            is_public: false,
            announcement: None,
            tags: Vec::new(),
        }
    }
}

/// 频道权限设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSettings {
    /// 是否允许除 owner 之外发送消息
    pub allow_member_post: bool,
    /// 是否全体禁言
    pub is_muted: bool,
    /// 是否公开频道（任何人可加入）
    pub is_public: bool,
    /// 是否允许成员邀请他人
    pub allow_member_invite: bool,
    /// 是否需要管理员审核加入
    pub require_approval: bool,
    /// 最大成员数量
    pub max_members: Option<u32>,
    /// 消息历史可见性
    pub history_visible: bool,
    /// 是否允许匿名发言
    pub allow_anonymous: bool,
}

impl Default for ChannelSettings {
    fn default() -> Self {
        Self {
            allow_member_post: true,
            is_muted: false,
            is_public: false,
            allow_member_invite: true,
            require_approval: false,
            max_members: None,
            history_visible: true,
            allow_anonymous: false,
        }
    }
}

// ============================================================================
// 频道核心结构
// ============================================================================

/// 会话核心结构
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    /// 频道ID
    pub id: u64, // 数据库中是 BIGINT
    /// 频道类型
    pub channel_type: ChannelType,
    /// 会话状态（数据库中没有此字段，从业务逻辑推断）
    pub status: ChannelStatus,
    /// 会话成员（从 privchat_channel_participants 表查询）
    pub members: HashMap<u64, ChannelMember>,
    /// 会话元数据（从 group_id 关联查询或从 metadata JSONB 字段）
    pub metadata: ChannelMetadata,
    /// 创建者ID（从 direct_user1_id 或 group.owner_id 获取）
    pub creator_id: u64,
    /// 数据库字段：私聊用户1 ID
    pub direct_user1_id: Option<u64>,
    /// 数据库字段：私聊用户2 ID
    pub direct_user2_id: Option<u64>,
    /// 数据库字段：群组ID
    pub group_id: Option<u64>,
    /// 最后一条消息ID（数据库中是 BIGINT）
    pub last_message_id: Option<u64>,
    /// 最后一条消息时间（数据库存储为 BIGINT 毫秒时间戳）
    pub last_message_at: Option<DateTime<Utc>>,
    /// 消息总数
    pub message_count: i64, // 改为 i64（数据库类型）
    /// 创建时间（数据库存储为 BIGINT 毫秒时间戳）
    pub created_at: DateTime<Utc>,
    /// 最后更新时间（数据库存储为 BIGINT 毫秒时间戳）
    pub updated_at: DateTime<Utc>,
    /// 频道权限设置（可选，用于扩展功能）
    pub settings: Option<ChannelSettings>,
}

impl Channel {
    /// 创建私聊会话
    pub fn new_direct(id: u64, user1_id: u64, user2_id: u64) -> Self {
        let mut members = HashMap::new();
        members.insert(user1_id, ChannelMember::new(user1_id, MemberRole::Member));
        members.insert(user2_id, ChannelMember::new(user2_id, MemberRole::Member));

        Self {
            id,
            channel_type: ChannelType::Direct,
            status: ChannelStatus::Active,
            members,
            metadata: ChannelMetadata::default(),
            creator_id: user1_id,
            direct_user1_id: Some(user1_id),
            direct_user2_id: Some(user2_id),
            group_id: None,
            last_message_id: None,
            last_message_at: None,
            message_count: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            settings: None,
        }
    }

    /// 从数据库行创建（处理时间戳和类型转换）
    pub fn from_db_row(
        channel_id: i64, // PostgreSQL BIGINT
        channel_type: i16,
        direct_user1_id: Option<i64>, // PostgreSQL BIGINT
        direct_user2_id: Option<i64>, // PostgreSQL BIGINT
        group_id: Option<i64>,        // PostgreSQL BIGINT
        last_message_id: Option<i64>, // PostgreSQL BIGINT (Snowflake)
        last_message_at: Option<i64>, // 毫秒时间戳
        message_count: i64,
        created_at: i64, // 毫秒时间戳
        updated_at: i64, // 毫秒时间戳
    ) -> Self {
        let conv_type = ChannelType::from_i16(channel_type);
        let creator_id = match conv_type {
            ChannelType::Direct => direct_user1_id.map(|id| id as u64).unwrap_or(0),
            ChannelType::Group => 0, // 需要从 group 表查询 owner_id
            ChannelType::System => 0,
        };

        Self {
            id: channel_id as u64,
            channel_type: conv_type,
            status: ChannelStatus::Active, // 默认活跃，需要从业务逻辑推断
            members: HashMap::new(),       // 需要单独查询
            metadata: ChannelMetadata::default(), // 需要从 group 表查询
            creator_id,
            direct_user1_id: direct_user1_id.map(|id| id as u64),
            direct_user2_id: direct_user2_id.map(|id| id as u64),
            group_id: group_id.map(|id| id as u64),
            last_message_id: last_message_id.map(|id| id as u64),
            last_message_at: last_message_at.and_then(|ts| DateTime::from_timestamp_millis(ts)),
            message_count,
            created_at: DateTime::from_timestamp_millis(created_at).unwrap_or_else(|| Utc::now()),
            updated_at: DateTime::from_timestamp_millis(updated_at).unwrap_or_else(|| Utc::now()),
            settings: None,
        }
    }

    /// 转换为数据库插入值
    pub fn to_db_values(
        &self,
    ) -> (
        i64,
        i16,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        i64,
        i64,
        i64,
    ) {
        (
            self.id as i64,
            self.channel_type.to_i16(),
            self.direct_user1_id.map(|id| id as i64),
            self.direct_user2_id.map(|id| id as i64),
            self.group_id.map(|id| id as i64),
            self.last_message_id.map(|id| id as i64),
            self.last_message_at.map(|dt| dt.timestamp_millis()),
            self.message_count,
            self.created_at.timestamp_millis(),
            self.updated_at.timestamp_millis(),
        )
    }

    /// 创建群聊会话
    pub fn new_group(id: u64, creator_id: u64, name: Option<String>) -> Self {
        let mut members = HashMap::new();
        members.insert(
            creator_id,
            ChannelMember::new(creator_id, MemberRole::Owner),
        );

        let mut metadata = ChannelMetadata::default();
        metadata.name = name;

        Self {
            id,
            channel_type: ChannelType::Group,
            status: ChannelStatus::Active,
            members,
            metadata,
            creator_id,
            direct_user1_id: None,
            direct_user2_id: None,
            group_id: Some(id), // ✨ 群聊的 group_id 等于 channel_id
            last_message_id: None,
            last_message_at: None,
            message_count: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            settings: None,
        }
    }

    /// 添加成员
    pub fn add_member(&mut self, user_id: u64, role: Option<MemberRole>) -> Result<(), String> {
        // 检查成员是否已存在
        if self.members.contains_key(&user_id) {
            return Err("Member already exists".to_string());
        }

        // 检查最大成员数限制
        if let Some(max_members) = self.metadata.max_members {
            if self.members.len() >= max_members {
                return Err("Maximum members limit reached".to_string());
            }
        }

        // 确定成员角色
        let member_role = role.unwrap_or(MemberRole::Member);

        // 对于私聊，只能是普通成员
        if self.channel_type == ChannelType::Direct && member_role != MemberRole::Member {
            return Err("Direct channels only support member role".to_string());
        }

        let member = ChannelMember::new(user_id, member_role);
        self.members.insert(user_id, member);
        self.updated_at = Utc::now();

        Ok(())
    }

    /// 移除成员
    pub fn remove_member(&mut self, user_id: &u64) -> Result<ChannelMember, String> {
        // 不能移除群主
        if let Some(member) = self.members.get(user_id) {
            if member.role == MemberRole::Owner {
                return Err("Cannot remove owner".to_string());
            }
        }

        // 私聊不能移除成员
        if self.channel_type == ChannelType::Direct {
            return Err("Cannot remove members from direct channels".to_string());
        }

        let member = self
            .members
            .remove(user_id)
            .ok_or_else(|| "Member not found".to_string())?;

        self.updated_at = Utc::now();
        Ok(member)
    }

    /// 更新成员角色
    pub fn update_member_role(
        &mut self,
        user_id: &u64,
        new_role: MemberRole,
    ) -> Result<(), String> {
        let member = self
            .members
            .get_mut(user_id)
            .ok_or_else(|| "Member not found".to_string())?;

        // 私聊不能修改角色
        if self.channel_type == ChannelType::Direct {
            return Err("Cannot change roles in direct channels".to_string());
        }

        // 不能修改群主角色
        if member.role == MemberRole::Owner {
            return Err("Cannot change owner role".to_string());
        }

        member.role = new_role.clone();
        member.permissions = match new_role {
            MemberRole::Owner => MemberPermissions::owner(),
            MemberRole::Admin => MemberPermissions::admin(),
            MemberRole::Member => MemberPermissions::member(),
        };

        self.updated_at = Utc::now();
        Ok(())
    }

    /// 检查用户权限
    pub fn check_permission(
        &self,
        user_id: &u64,
        permission: fn(&MemberPermissions) -> bool,
    ) -> bool {
        self.members
            .get(user_id)
            .map(|member| permission(&member.permissions))
            .unwrap_or(false)
    }

    /// 获取所有在线成员ID
    pub fn get_member_ids(&self) -> Vec<u64> {
        self.members.keys().cloned().collect()
    }

    /// 更新最后消息信息
    pub fn update_last_message(&mut self, message_id: u64) {
        self.last_message_id = Some(message_id);
        self.last_message_at = Some(Utc::now());
        self.message_count += 1;
        self.updated_at = Utc::now();
    }

    /// 标记成员已读
    pub fn mark_member_read(&mut self, user_id: &u64, message_id: u64) -> Result<(), String> {
        let member = self
            .members
            .get_mut(user_id)
            .ok_or_else(|| "Member not found".to_string())?;

        member.mark_read(message_id);
        Ok(())
    }

    /// 获取未读成员数
    pub fn get_unread_count(&self) -> usize {
        if let Some(_last_msg_id) = self.last_message_id {
            // 注意：这里需要比较 u64 message_id，但 ChannelMember 使用 u64
            // 暂时返回所有成员（需要后续优化）
            self.members
                .values()
                .filter(|member| member.last_read_message_id.is_none())
                .count()
        } else {
            0
        }
    }

    /// 是否为活跃会话
    pub fn is_active(&self) -> bool {
        self.status == ChannelStatus::Active
    }

    /// 归档会话
    pub fn archive(&mut self) {
        self.status = ChannelStatus::Archived;
        self.updated_at = Utc::now();
    }

    /// 恢复会话
    pub fn unarchive(&mut self) {
        self.status = ChannelStatus::Active;
        self.updated_at = Utc::now();
    }

    /// 获取频道显示名称
    pub fn get_display_name(&self) -> String {
        match &self.metadata.name {
            Some(name) => name.clone(),
            None => match self.channel_type {
                ChannelType::Direct => {
                    let members: Vec<String> =
                        self.members.keys().map(|id| id.to_string()).collect();
                    format!("私聊 ({})", members.join(", "))
                }
                ChannelType::Group => "群聊".to_string(),
                ChannelType::System => "系统频道".to_string(),
            },
        }
    }

    /// 检查用户是否可以发送消息
    pub fn can_user_post(&self, user_id: &u64) -> bool {
        // 检查用户是否在成员列表中
        if let Some(member) = self.members.get(user_id) {
            // 检查频道设置
            if let Some(settings) = &self.settings {
                if !settings.allow_member_post && member.role != MemberRole::Owner {
                    return false;
                }
            }
            // 检查成员权限
            member.permissions.can_send_message
        } else {
            false
        }
    }
}

// ============================================================================
// 用户会话视图
// ============================================================================

/// 用户会话视图 - 个人维度的会话管理
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserChannelView {
    /// 用户ID
    pub user_id: UserId,
    /// 频道ID
    pub channel_id: ChannelId,
    /// 最后已读消息ID
    pub last_read_message_id: Option<MessageId>,
    /// 是否静音
    pub is_muted: bool,
    /// 是否置顶
    pub is_pinned: bool,
    /// 未读消息数
    pub unread_count: u32,
    /// 用户备注（对频道的个人备注）
    pub remark: Option<String>,
    /// 自定义标题
    pub custom_title: Option<String>,
    /// 最后查看时间
    pub last_viewed_at: DateTime<Utc>,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 更新时间
    pub updated_at: DateTime<Utc>,
    /// 是否隐藏
    pub is_hidden: bool,
    /// 自定义排序权重
    pub sort_weight: i32,
}

impl UserChannelView {
    /// 创建新的用户会话视图
    pub fn new(user_id: UserId, channel_id: ChannelId) -> Self {
        Self {
            user_id,
            channel_id,
            last_read_message_id: None,
            is_muted: false,
            is_pinned: false,
            unread_count: 0,
            remark: None,
            custom_title: None,
            last_viewed_at: Utc::now(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_hidden: false,
            sort_weight: 0,
        }
    }

    /// 标记消息为已读
    pub fn mark_as_read(&mut self, message_id: MessageId) {
        self.last_read_message_id = Some(message_id);
        self.unread_count = 0;
        self.last_viewed_at = Utc::now();
        self.updated_at = Utc::now();
    }

    /// 增加未读消息数
    pub fn increment_unread(&mut self) {
        if !self.is_muted {
            self.unread_count += 1;
            self.updated_at = Utc::now();
        }
    }

    /// 设置静音状态
    pub fn set_muted(&mut self, muted: bool) {
        self.is_muted = muted;
        if muted {
            self.unread_count = 0; // 静音时清零未读数
        }
        self.updated_at = Utc::now();
    }

    /// 设置置顶状态
    pub fn set_pinned(&mut self, pinned: bool) {
        self.is_pinned = pinned;
        self.updated_at = Utc::now();
    }

    /// 设置备注
    pub fn set_remark(&mut self, remark: Option<String>) {
        self.remark = remark;
        self.updated_at = Utc::now();
    }
}

// ============================================================================
// 请求和响应类型
// ============================================================================

/// 会话创建请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateChannelRequest {
    /// 会话类型
    pub channel_type: ChannelType,
    /// 会话名称（群聊时必需）
    pub name: Option<String>,
    /// 会话描述
    pub description: Option<String>,
    /// 初始成员ID列表（不包括创建者）
    pub member_ids: Vec<u64>,
    /// 是否公开
    pub is_public: Option<bool>,
    /// 最大成员数
    pub max_members: Option<usize>,
}

/// 会话操作响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelResponse {
    /// 会话信息
    pub channel: Channel,
    /// 操作是否成功
    pub success: bool,
    /// 错误信息
    pub error: Option<String>,
}

/// 会话列表响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelListResponse {
    /// 会话列表
    pub channels: Vec<Channel>,
    /// 总数
    pub total: usize,
    /// 是否还有更多
    pub has_more: bool,
}

// ============================================================================
// 群组设置（简化版，参考微信）
// ============================================================================

/// 群组设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupSettings {
    // === 加群设置 ===
    /// 进群需要审批（简单开关）
    pub join_need_approval: bool,

    /// 群成员可以邀请（简单开关）
    pub member_can_invite: bool,

    // === 功能设置 ===
    /// 全员禁言
    pub all_muted: bool,

    /// 群成员数量上限
    pub max_members: u32,

    // === 群信息 ===
    /// 群公告（可选）
    pub announcement: Option<String>,

    /// 群描述（可选）
    pub description: Option<String>,

    // === 创建时间 ===
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Default for GroupSettings {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            // 默认无需审批
            join_need_approval: false,
            // 默认群成员不能邀请（仅Owner/Admin）
            member_can_invite: false,
            // 默认不禁言
            all_muted: false,
            // 默认500人
            max_members: 500,
            // 无公告
            announcement: None,
            description: None,
            created_at: now,
            updated_at: now,
        }
    }
}

impl GroupSettings {
    /// 创建新的群组设置
    pub fn new() -> Self {
        Self::default()
    }

    /// 更新群组设置
    pub fn update(&mut self) {
        self.updated_at = Utc::now();
    }
}

// ============================================================================
// 加群审批（可选功能）
// ============================================================================

/// 加群申请状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum JoinRequestStatus {
    /// 待审批
    Pending = 0,
    /// 已同意
    Approved = 1,
    /// 已拒绝
    Rejected = 2,
    /// 已过期
    Expired = 3,
}

/// 加群方式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JoinMethod {
    /// 群成员邀请
    Invite { inviter_id: String },
    /// 群二维码
    QRCode { qr_code: String },
}

/// 加群申请记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    /// 申请ID
    pub request_id: String,
    /// 群组ID
    pub group_id: u64,
    /// 申请人ID
    pub user_id: u64,
    /// 加群方式
    pub method: JoinMethod,
    /// 申请理由（可选）
    pub message: Option<String>,
    /// 申请状态
    pub status: JoinRequestStatus,
    /// 审批人ID（可选）
    pub approver_id: Option<u64>,
    /// 审批理由（可选）
    pub approval_message: Option<String>,
    /// 申请时间
    pub created_at: DateTime<Utc>,
    /// 审批时间（可选）
    pub approved_at: Option<DateTime<Utc>>,
}

// ============================================================================
// 统计和搜索
// ============================================================================

/// 频道统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelStats {
    /// 频道ID
    pub channel_id: ChannelId,
    /// 成员数量
    pub member_count: u32,
    /// 消息总数
    pub message_count: u64,
    /// 今日消息数
    pub today_message_count: u32,
    /// 活跃成员数（最近7天）
    pub active_member_count: u32,
    /// 统计时间
    pub stats_time: DateTime<Utc>,
}

/// 频道搜索结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSearchResult {
    /// 频道信息
    pub channel: Channel,
    /// 匹配得分
    pub score: f32,
    /// 匹配的字段
    pub matched_fields: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_direct_channel() {
        let conv = Channel::new_direct(123, 1, 2);

        assert_eq!(conv.channel_type, ChannelType::Direct);
        assert_eq!(conv.members.len(), 2);
        assert!(conv.members.contains_key(&1));
        assert!(conv.members.contains_key(&2));
    }

    #[test]
    fn test_create_group_channel() {
        let conv = Channel::new_group(123, 1, Some("Test Group".to_string()));

        assert_eq!(conv.channel_type, ChannelType::Group);
        assert_eq!(conv.members.len(), 1);
        assert_eq!(conv.members.get(&1).unwrap().role, MemberRole::Owner);
        assert_eq!(conv.metadata.name, Some("Test Group".to_string()));
    }

    #[test]
    fn test_add_member_to_group() {
        let mut conv = Channel::new_group(123, 1, None);

        assert!(conv.add_member(2, None).is_ok());
        assert_eq!(conv.members.len(), 2);
        assert_eq!(conv.members.get(&2).unwrap().role, MemberRole::Member);

        // 不能添加重复成员
        assert!(conv.add_member(2, None).is_err());
    }

    #[test]
    fn test_member_permissions() {
        let owner_perms = MemberPermissions::owner();
        assert!(owner_perms.can_manage_permissions);
        assert!(owner_perms.can_send_message);

        let member_perms = MemberPermissions::member();
        assert!(!member_perms.can_manage_permissions);
        assert!(member_perms.can_send_message);

        // 注意：MemberPermissions 没有 guest() 方法，已删除此测试
    }
}
