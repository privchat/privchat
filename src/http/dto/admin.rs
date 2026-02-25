//! Admin API 的 DTO（请求/响应结构体）
//!
//! 所有 Admin API handler 使用强类型 DTO 而非手写 json!({})

use serde::{Deserialize, Serialize};

// =====================================================
// 通用查询参数
// =====================================================

/// 分页查询参数
#[derive(Debug, Deserialize, Default)]
pub struct PageParams {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

/// 会话列表查询参数
#[derive(Debug, Deserialize, Default)]
pub struct ChannelListParams {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub channel_type: Option<i16>,
    pub user_id: Option<u64>,
}

// =====================================================
// 通用响应
// =====================================================

/// 通用操作成功响应
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: String,
}

impl SuccessResponse {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
        }
    }
}

// =====================================================
// 用户封禁/解封
// =====================================================

/// 封禁用户请求
#[derive(Debug, Deserialize)]
pub struct SuspendUserRequest {
    /// 封禁原因（可选）
    pub reason: Option<String>,
    /// 封禁时长，秒。None 表示永久封禁
    pub duration_secs: Option<u64>,
}

/// 封禁用户响应
#[derive(Debug, Serialize)]
pub struct SuspendUserResponse {
    pub success: bool,
    pub user_id: u64,
    pub previous_status: i16,
    pub current_status: i16,
    pub reason: Option<String>,
    pub revoked_devices: usize,
    pub message: String,
}

/// 解封用户响应
#[derive(Debug, Serialize)]
pub struct UnsuspendUserResponse {
    pub success: bool,
    pub user_id: u64,
    pub previous_status: i16,
    pub current_status: i16,
    pub message: String,
}

// =====================================================
// 设备强制踢出
// =====================================================

/// 踢出设备请求
#[derive(Debug, Deserialize)]
pub struct RevokeDeviceRequest {
    /// 设备所属用户 ID
    pub user_id: u64,
    /// 踢出原因（可选）
    pub reason: Option<String>,
}

/// 踢出设备响应
#[derive(Debug, Serialize)]
pub struct RevokeDeviceResponse {
    pub success: bool,
    pub device_id: String,
    pub user_id: u64,
    pub message: String,
}

/// 撤销用户全部设备请求
#[derive(Debug, Deserialize)]
pub struct RevokeAllDevicesRequest {
    /// 撤销原因（可选）
    pub reason: Option<String>,
}

/// 撤销用户全部设备响应
#[derive(Debug, Serialize)]
pub struct RevokeAllDevicesResponse {
    pub success: bool,
    pub user_id: u64,
    pub revoked_count: usize,
    pub message: String,
}

// =====================================================
// 群组成员管理
// =====================================================

/// 群组成员项
#[derive(Debug, Serialize)]
pub struct GroupMemberItem {
    pub user_id: u64,
    pub role: String,
    pub joined_at: Option<i64>,
    pub nickname: Option<String>,
}

/// 群组成员列表响应
#[derive(Debug, Serialize)]
pub struct ListGroupMembersResponse {
    pub group_id: u64,
    pub members: Vec<GroupMemberItem>,
    pub total: usize,
}

/// 移除群组成员响应
#[derive(Debug, Serialize)]
pub struct RemoveGroupMemberResponse {
    pub success: bool,
    pub group_id: u64,
    pub user_id: u64,
    pub message: String,
}

// =====================================================
// 消息撤回 + 系统消息
// =====================================================

/// 管理员撤回消息请求
#[derive(Debug, Deserialize)]
pub struct RevokeMessageRequest {
    /// 撤回原因（可选）
    pub reason: Option<String>,
}

/// 管理员撤回消息响应
#[derive(Debug, Serialize)]
pub struct RevokeMessageResponse {
    pub success: bool,
    pub message_id: u64,
    pub channel_id: u64,
    pub revoked_at: i64,
    pub message: String,
}

/// 发送系统消息请求
#[derive(Debug, Deserialize)]
pub struct SendSystemMessageRequest {
    /// 目标频道 ID
    pub channel_id: u64,
    /// 消息内容
    pub content: String,
    /// 消息类型（可选，默认 "text"）
    pub message_type: Option<String>,
    /// 附加元数据（可选）
    pub metadata: Option<serde_json::Value>,
}

/// 发送系统消息响应
#[derive(Debug, Serialize)]
pub struct SendSystemMessageResponse {
    pub success: bool,
    pub message_id: u64,
    pub channel_id: u64,
    pub created_at: i64,
    pub message: String,
}

// =====================================================
// 安全管控
// =====================================================

/// Shadow Ban 用户项
#[derive(Debug, Serialize)]
pub struct ShadowBannedItem {
    pub user_id: u64,
    pub device_id: String,
    pub state: String,
    pub trust_score: Option<u32>,
}

/// Shadow Ban 列表响应
#[derive(Debug, Serialize)]
pub struct ListShadowBannedResponse {
    pub users: Vec<ShadowBannedItem>,
    pub total: usize,
}

/// 解除 Shadow Ban 响应
#[derive(Debug, Serialize)]
pub struct UnshadowBanResponse {
    pub success: bool,
    pub user_id: u64,
    pub affected_devices: usize,
    pub message: String,
}

/// 用户安全状态响应
#[derive(Debug, Serialize)]
pub struct UserSecurityStateResponse {
    pub user_id: u64,
    pub devices: Vec<DeviceSecurityState>,
}

/// 设备安全状态
#[derive(Debug, Serialize)]
pub struct DeviceSecurityState {
    pub device_id: String,
    pub state: String,
    pub trust_score: Option<u32>,
}

/// 重置安全状态响应
#[derive(Debug, Serialize)]
pub struct ResetSecurityStateResponse {
    pub success: bool,
    pub user_id: u64,
    pub affected_devices: usize,
    pub message: String,
}

// =====================================================
// 管理端发送消息（指定发送者）
// =====================================================

/// 管理端发送消息请求（可指定发送者）
#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    /// 目标频道 ID
    pub channel_id: u64,
    /// 发送者用户 ID
    pub sender_id: u64,
    /// 消息内容
    pub content: String,
    /// 消息类型（可选，默认 "text"）
    pub message_type: Option<String>,
    /// 附加元数据（可选）
    pub metadata: Option<serde_json::Value>,
}

/// 管理端发送消息响应
#[derive(Debug, Serialize)]
pub struct SendMessageResponse {
    pub success: bool,
    pub message_id: u64,
    pub channel_id: u64,
    pub sender_id: u64,
    pub created_at: i64,
    pub message: String,
}

// =====================================================
// 管理端添加群成员
// =====================================================

/// 添加群成员请求
#[derive(Debug, Deserialize)]
pub struct AddGroupMemberRequest {
    /// 要添加的用户 ID
    pub user_id: u64,
}

/// 添加群成员响应
#[derive(Debug, Serialize)]
pub struct AddGroupMemberResponse {
    pub success: bool,
    pub group_id: u64,
    pub user_id: u64,
    pub announcement_message_id: Option<u64>,
    pub message: String,
}

// =====================================================
// 好友管理
// =====================================================

/// 创建好友关系请求
#[derive(Debug, Deserialize)]
pub struct CreateFriendshipRequest {
    /// 用户1 ID
    pub user1_id: u64,
    /// 用户2 ID
    pub user2_id: u64,
    /// 来源（可选，用于记录好友关系来源）
    pub source: Option<String>,
}

/// 创建好友关系响应
#[derive(Debug, Serialize)]
pub struct CreateFriendshipResponse {
    pub success: bool,
    pub user1_id: u64,
    pub user2_id: u64,
    pub channel_id: u64,
    pub message: String,
}

// =====================================================
// 在线状态
// =====================================================

/// 在线人数响应
#[derive(Debug, Serialize)]
pub struct OnlineCountResponse {
    pub online_count: usize,
}

/// 在线用户列表项
#[derive(Debug, Serialize)]
pub struct OnlineUserItem {
    pub user_id: u64,
    pub username: Option<String>,
    pub nickname: Option<String>,
    pub device_count: usize,
    pub devices: Vec<OnlineDeviceItem>,
}

/// 在线设备项
#[derive(Debug, Serialize)]
pub struct OnlineDeviceItem {
    pub device_id: String,
    pub device_type: Option<String>,
    pub device_name: Option<String>,
    pub ip_address: Option<String>,
    pub connected_at: i64,
    pub last_active: Option<i64>,
}

/// 在线用户列表响应
#[derive(Debug, Serialize)]
pub struct ListOnlineUsersResponse {
    pub users: Vec<OnlineUserItem>,
    pub total: usize,
    pub page: u32,
    pub page_size: u32,
}

/// 用户连接详情响应
#[derive(Debug, Serialize)]
pub struct UserConnectionResponse {
    pub user_id: u64,
    pub username: Option<String>,
    pub nickname: Option<String>,
    pub online: bool,
    pub devices: Vec<OnlineDeviceItem>,
}

// =====================================================
// 会话管理
// =====================================================

/// 会话列表项
#[derive(Debug, Serialize)]
pub struct ChannelItem {
    pub channel_id: u64,
    pub channel_type: i16,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub member_count: Option<i32>,
    pub last_message: Option<ChannelLastMessage>,
    pub created_at: i64,
}

/// 会话最后消息
#[derive(Debug, Serialize)]
pub struct ChannelLastMessage {
    pub message_id: i64,
    pub sender_id: i64,
    pub content: String,
    pub message_type: i16,
    pub timestamp: i64,
}

/// 会话列表响应
#[derive(Debug, Serialize)]
pub struct ListChannelsResponse {
    pub channels: Vec<ChannelItem>,
    pub total: usize,
    pub page: u32,
    pub page_size: u32,
}

/// 会话参与者项
#[derive(Debug, Serialize)]
pub struct ChannelParticipantItem {
    pub user_id: i64,
    pub username: Option<String>,
    pub nickname: Option<String>,
    pub role: Option<String>,
}

/// 会话参与者列表响应
#[derive(Debug, Serialize)]
pub struct ListChannelParticipantsResponse {
    pub channel_id: u64,
    pub participants: Vec<ChannelParticipantItem>,
    pub total: usize,
}

// =====================================================
// 全局广播
// =====================================================

/// 全局广播请求
#[derive(Debug, Deserialize)]
pub struct BroadcastRequest {
    /// 广播内容
    pub content: String,
    /// 目标范围: "all" | "active"
    pub target_scope: Option<String>,
    /// 消息类型 (默认 5 = 系统消息)
    pub message_type: Option<i16>,
}

/// 全局广播响应
#[derive(Debug, Serialize)]
pub struct BroadcastResponse {
    pub success: bool,
    pub target_scope: String,
    pub online_recipients: usize,
    pub message: String,
}

// =====================================================
// 消息搜索
// =====================================================

/// 消息搜索请求
#[derive(Debug, Deserialize)]
pub struct SearchMessagesRequest {
    /// 搜索关键词 (必填)
    pub keyword: String,
    /// 频道 ID (可选)
    pub channel_id: Option<u64>,
    /// 用户 ID (可选)
    pub user_id: Option<u64>,
    /// 消息类型 (可选)
    pub message_type: Option<i16>,
    /// 开始时间 (可选)
    pub start_time: Option<i64>,
    /// 结束时间 (可选)
    pub end_time: Option<i64>,
    /// 页码 (可选)
    pub page: Option<u32>,
    /// 每页数量 (可选)
    pub page_size: Option<u32>,
}

/// 消息搜索项
#[derive(Debug, Serialize)]
pub struct SearchMessageItem {
    pub message_id: i64,
    pub channel_id: i64,
    pub sender_id: i64,
    pub content: String,
    pub message_type: i16,
    pub created_at: i64,
}

/// 消息搜索响应
#[derive(Debug, Serialize)]
pub struct SearchMessagesResponse {
    pub messages: Vec<SearchMessageItem>,
    pub total: usize,
    pub page: u32,
    pub page_size: u32,
}

// =====================================================
// 系统运维
// =====================================================

/// 健康检查响应
#[derive(Debug, Serialize)]
pub struct HealthCheckResponse {
    pub status: String,
    pub version: String,
    pub uptime_secs: u64,
    pub connections: usize,
}
