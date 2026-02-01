// 业务服务层模块
pub mod auth_service;
pub mod channel_service;  // ChannelService 在这里
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

pub use auth_service::AuthService;
pub use channel_service::{ChannelService, ChannelServiceConfig, ChannelServiceStats, EnhancedChannelItem, EnhancedChannelListResponse, LastMessagePreview};
pub use friend_service::FriendService;
pub use group_service::GroupService;
pub use message_service::MessageService;
pub use notification_service::NotificationService;
pub use presence_service::PresenceService;
pub use push_service::PushService;
pub use user_service::UserService;
pub use message_history_service::{MessageHistoryService, MessageHistoryRecord, MessageQueryParams, ChannelMessageStats, ReplyMessagePreview};
pub use privacy_service::{PrivacyService, PrivacySettingsUpdate};
pub use read_receipt_service::{ReadReceiptService, ReadReceipt, GroupReadStats};
pub use file_service::{FileService, FileMetadata, FileType, FileUrlResponse};
pub use upload_token_service::{UploadTokenService, UploadToken};
pub use sticker_service::{StickerService, Sticker, StickerPackage};
pub use blacklist_service::{BlacklistService, BlacklistEntry};
pub use qrcode_service::QRCodeService;
pub use approval_service::{ApprovalService, JoinRequest, JoinRequestStatus, JoinMethod};
pub use offline_queue_service::OfflineQueueService;
pub use unread_count_service::UnreadCountService;
pub use message_dedup_service::MessageDedupService;
pub use reaction_service::{ReactionService, Reaction, ReactionStats};
pub use mention_service::MentionService;