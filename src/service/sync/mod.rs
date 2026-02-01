/// Phase 8: 同步服务模块
/// 
/// 模块组织：
/// - sync_service: 核心同步服务（RPC 处理）
/// - commit_dao: Commit Log 数据库操作
/// - pts_dao: Channel pts 数据库操作
/// - registry_dao: 客户端消息号注册表操作
/// - sync_cache: Redis 缓存操作

pub mod sync_service;
pub mod commit_dao;
pub mod pts_dao;
pub mod registry_dao;
pub mod sync_cache;

// 重新导出
pub use sync_service::SyncService;
pub use commit_dao::CommitLogDao;
pub use pts_dao::ChannelPtsDao;
pub use registry_dao::ClientMsgRegistryDao;
pub use sync_cache::SyncCache;
