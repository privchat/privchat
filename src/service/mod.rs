// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// 业务服务层模块
pub mod admin_service;
pub mod auth_service;
pub mod channel_service; // ChannelService 在这里
pub mod friend_service;
pub mod group_service;
pub mod message_service;
pub mod notification_service;
pub mod presence_service;
pub mod push_service;
// pub mod sync_service; // 已废弃，已迁移到 sync/sync_service.rs
pub mod sync; // Phase 8 同步服务（P0/P1/P2全部完成）
pub mod user_service;

// 新增频道服务（已合并到 channel_service，不再单独使用）
// pub mod channel_service;
// 新增消息历史服务
pub mod message_history_service;
// 新增隐私服务
pub mod privacy_service;
// 新增已读回执服务
pub mod read_receipt_service;
// read_pts 单一路径服务
pub mod read_state_service;
// 新增文件服务
pub mod file_service;
// 新增上传 token 服务
pub mod upload_token_service;
// 新增表情包服务
pub mod sticker_service;
// 新增黑名单服务
pub mod blacklist_service;
// 新增二维码服务
pub mod qrcode_service;
// 新增审批服务
pub mod approval_service;
// 新增离线消息队列服务
pub mod offline_queue_service;
// 新增未读计数服务
pub mod unread_count_service;
// 新增消息去重服务
pub mod message_dedup_service;
// 新增 Reaction 服务
pub mod reaction_service;
// 新增 @提及服务
pub mod mention_service;
// Room 订阅历史服务（Redis）
pub mod room_history_service;
// 送达水位追踪服务
pub mod delivery_tracker;

pub use admin_service::AdminService;
pub use approval_service::{ApprovalService, JoinMethod, JoinRequest, JoinRequestStatus};
pub use auth_service::AuthService;
pub use blacklist_service::{BlacklistEntry, BlacklistService};
pub use channel_service::{
    ChannelService, ChannelServiceConfig, ChannelServiceStats, EnhancedChannelItem,
    EnhancedChannelListResponse, LastMessagePreview,
};
pub use file_service::{FileMetadata, FileService, FileType, FileUrlResponse};
pub use friend_service::FriendService;
pub use group_service::GroupService;
pub use mention_service::MentionService;
pub use message_dedup_service::MessageDedupService;
pub use message_history_service::{
    ChannelMessageStats, MessageHistoryRecord, MessageHistoryService, MessageQueryParams,
    ReplyMessagePreview,
};
pub use message_service::{
    MessageService, RevokedMessageSummary, ServerSendMessageRequest, ServerSendMessageResult,
};
pub use notification_service::NotificationService;
pub use offline_queue_service::OfflineQueueService;
pub use presence_service::PresenceService;
pub use privacy_service::{PrivacyService, PrivacySettingsUpdate};
pub use push_service::PushService;
pub use qrcode_service::QRCodeService;
pub use reaction_service::{Reaction, ReactionService, ReactionStats};
pub use read_receipt_service::{GroupReadStats, ReadReceipt, ReadReceiptService};
pub use read_state_service::{ChannelReadCursorRow, ReadPtsUpdateResult, ReadStateService};
pub use room_history_service::RoomHistoryService;
pub use sticker_service::{Sticker, StickerPackage, StickerService};
pub use unread_count_service::UnreadCountService;
pub use upload_token_service::{UploadToken, UploadTokenService};
pub use user_service::{CreateUserAdminParams, UpdateUserAdminParams, UserService};
pub use delivery_tracker::DeliveryTracker;
