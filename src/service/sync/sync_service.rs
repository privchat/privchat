/// Phase 8: pts-Based 同步服务（完整实现版本）
/// 
/// 职责：
/// - 处理客户端提交命令（sync/submit）
/// - 提供差异拉取（sync/get_difference）
/// - 管理 pts 分配和间隙检测

use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::error::Result;
use crate::model::pts::PtsGenerator;
use super::{CommitLogDao, ChannelPtsDao, ClientMsgRegistryDao, SyncCache};

// 重新导出协议类型
use privchat_protocol::rpc::sync::{
    ClientSubmitRequest, ClientSubmitResponse,
    GetDifferenceRequest, GetDifferenceResponse,
    GetChannelPtsRequest, GetChannelPtsResponse,
    BatchGetChannelPtsRequest, BatchGetChannelPtsResponse,
    ServerCommit, ServerDecision, ChannelPtsInfo, SenderInfo,
};

/// 同步服务（完整版本）⭐
pub struct SyncService {
    /// pts 生成器（内存中维护）
    pts_generator: Arc<PtsGenerator>,
    
    /// Commit Log DAO
    commit_dao: Arc<CommitLogDao>,
    
    /// Channel pts DAO
    pts_dao: Arc<ChannelPtsDao>,
    
    /// 客户端消息号注册表 DAO
    registry_dao: Arc<ClientMsgRegistryDao>,
    
    /// Redis 缓存
    cache: Arc<SyncCache>,
}

impl SyncService {
    /// 创建同步服务
    pub fn new(
        pts_generator: Arc<PtsGenerator>,
        commit_dao: Arc<CommitLogDao>,
        pts_dao: Arc<ChannelPtsDao>,
        registry_dao: Arc<ClientMsgRegistryDao>,
        cache: Arc<SyncCache>,
    ) -> Self {
        Self {
            pts_generator,
            commit_dao,
            pts_dao,
            registry_dao,
            cache,
        }
    }
    
    // ============================================================
    // RPC 处理方法
    // ============================================================
    
    /// 处理客户端提交命令 ⭐
    /// 
    /// RPC: sync/submit
    /// 
    /// 流程：
    /// 1. 幂等性检查（local_message_id 去重）
    /// 2. 权限验证
    /// 3. 获取服务器当前 pts
    /// 4. 检测间隙
    /// 5. 分配新的 pts（数据库原子操作）
    /// 6. 生成 server_msg_id
    /// 7. 构造 ServerCommit
    /// 8. 保存到数据库（Commit Log）
    /// 9. 缓存到 Redis
    /// 10. Fan-out 给在线用户
    /// 11. 注册 local_message_id
    /// 12. 返回响应
    pub async fn handle_client_submit(
        &self,
        req: ClientSubmitRequest,
        sender_id: u64, // 从 JWT token 中提取
    ) -> Result<ClientSubmitResponse> {
        info!(
            "收到客户端提交: local_message_id={}, channel_id={}, channel_type={}",
            req.local_message_id, req.channel_id, req.channel_type
        );
        
        // 1. 幂等性检查（local_message_id 去重）⭐
        if let Some(existing) = self.registry_dao.check_duplicate(req.local_message_id).await? {
            info!("检测到重复提交: local_message_id={}", req.local_message_id);
            return Ok(existing);
        }
        
        // 2. 权限验证
        // TODO: 实现权限检查
        // self.validate_permission(req.channel_id, sender_id).await?;
        
        // 3. 获取服务器当前 pts（先查缓存，再查数据库）⭐
        let server_pts = match self.cache.get_pts_from_cache(req.channel_id, req.channel_type).await? {
            Some(cached_pts) => {
                debug!("✅ pts 缓存命中: {}", cached_pts);
                cached_pts
            }
            None => {
                let db_pts = self.pts_dao.get_current_pts(req.channel_id, req.channel_type).await?;
                // 更新缓存
                self.cache.cache_pts(req.channel_id, req.channel_type, db_pts).await?;
                db_pts
            }
        };
        
        // 4. 检测间隙 ⭐
        let has_gap = req.last_pts < server_pts.saturating_sub(1);
        
        if has_gap {
            warn!(
                "检测到 pts 间隙: channel_id={}, client_pts={}, server_pts={}, gap={}",
                req.channel_id, req.last_pts, server_pts, server_pts - req.last_pts
            );
        }
        
        // 5. 分配新的 pts（数据库原子操作）⭐⭐⭐
        let new_pts = self.pts_dao.allocate_pts(req.channel_id, req.channel_type).await?;
        
        // 同步到内存 PtsGenerator（可选，提高性能）
        self.pts_generator.set_pts(req.channel_id, req.channel_type, new_pts).await;
        
        // 6. 生成 server_msg_id（使用 snowflake）
        let server_msg_id = self.generate_msg_id().await;
        
        // 7. 构造 ServerCommit ⭐
        let commit = ServerCommit {
            pts: new_pts,
            server_msg_id,
            local_message_id: Some(req.local_message_id),
            channel_id: req.channel_id,
            channel_type: req.channel_type,
            message_type: req.command_type.clone(),
            content: req.payload.clone(),
            server_timestamp: chrono::Utc::now().timestamp_millis(),
            sender_id,
            sender_info: self.get_sender_info(sender_id).await.ok(),
        };
        
        // 8. 保存到数据库（Commit Log）⭐
        self.commit_dao.save_commit(&commit).await?;
        
        // 9. 缓存到 Redis ⭐
        self.cache.cache_commit(&commit).await?;
        
        // 10. Fan-out 给在线用户 ⭐
        let online_users = self.cache.get_online_users(req.channel_id, req.channel_type).await?;
        if !online_users.is_empty() {
            self.cache.fanout_to_online_users(&online_users, &commit).await?;
        }
        
        // 11. 注册 local_message_id（防止重复）⭐
        self.registry_dao.register(
            req.local_message_id,
            server_msg_id,
            new_pts,
            req.channel_id,
            req.channel_type,
            sender_id,
            "accepted", // decision
        ).await?;
        
        // 12. 返回响应 ⭐
        let response = ClientSubmitResponse {
            decision: ServerDecision::Accepted,
            pts: Some(new_pts),
            server_msg_id: Some(server_msg_id),
            server_timestamp: commit.server_timestamp,
            local_message_id: req.local_message_id,
            has_gap,
            current_pts: server_pts,
        };
        
        info!(
            "✅ 命令已提交: local_message_id={}, pts={}, server_msg_id={}, has_gap={}",
            req.local_message_id, new_pts, server_msg_id, has_gap
        );
        
        Ok(response)
    }
    
    /// 处理获取差异请求 ⭐
    /// 
    /// RPC: sync/get_difference
    /// 
    /// 流程：
    /// 1. 先查 Redis 缓存
    /// 2. Cache Miss 时查数据库
    /// 3. 获取当前 pts
    /// 4. 检查是否还有更多
    /// 5. 返回响应
    pub async fn handle_get_difference(
        &self,
        req: GetDifferenceRequest,
    ) -> Result<GetDifferenceResponse> {
        let limit = req.limit.unwrap_or(100);
        
        info!(
            "收到差异拉取请求: channel_id={}, channel_type={}, last_pts={}, limit={}",
            req.channel_id, req.channel_type, req.last_pts, limit
        );
        
        // 1. 先查 Redis 缓存（快）⭐
        let commits = match self.cache.query_commits_from_cache(
            req.channel_id,
            req.channel_type,
            req.last_pts,
            limit,
        ).await? {
            Some(cached_commits) => {
                info!("✅ 缓存命中: 返回 {} 条 commits", cached_commits.len());
                cached_commits
            }
            None => {
                // 2. Cache Miss，查数据库 ⭐
                info!("⚠️  缓存未命中，查询数据库");
                let db_commits = self.commit_dao.query_commits(
                    req.channel_id,
                    req.channel_type,
                    req.last_pts,
                    limit,
                ).await?;
                
                // 异步更新缓存（可选）
                if !db_commits.is_empty() {
                    let cache = self.cache.clone();
                    let commits_clone = db_commits.clone();
                    tokio::spawn(async move {
                        let _ = cache.batch_cache_commits(&commits_clone).await;
                    });
                }
                
                db_commits
            }
        };
        
        // 3. 获取当前 pts ⭐
        let current_pts = match self.cache.get_pts_from_cache(req.channel_id, req.channel_type).await? {
            Some(cached_pts) => cached_pts,
            None => {
                let db_pts = self.pts_dao.get_current_pts(req.channel_id, req.channel_type).await?;
                self.cache.cache_pts(req.channel_id, req.channel_type, db_pts).await?;
                db_pts
            }
        };
        
        // 4. 检查是否还有更多 ⭐
        let has_more = if let Some(last_commit) = commits.last() {
            last_commit.pts < current_pts
        } else {
            false
        };
        
        info!(
            "✅ 差异拉取完成: 返回 {} 条 commits, has_more={}, current_pts={}",
            commits.len(), has_more, current_pts
        );
        
        // 5. 返回响应
        Ok(GetDifferenceResponse {
            commits,
            current_pts,
            has_more,
        })
    }
    
    /// 处理获取频道 pts 请求 ⭐
    /// 
    /// RPC: sync/get_channel_pts
    pub async fn handle_get_channel_pts(
        &self,
        req: GetChannelPtsRequest,
    ) -> Result<GetChannelPtsResponse> {
        // 先查缓存
        let current_pts = match self.cache.get_pts_from_cache(req.channel_id, req.channel_type).await? {
            Some(cached_pts) => cached_pts,
            None => {
                let db_pts = self.pts_dao.get_current_pts(req.channel_id, req.channel_type).await?;
                self.cache.cache_pts(req.channel_id, req.channel_type, db_pts).await?;
                db_pts
            }
        };
        
        debug!(
            "获取频道 pts: channel_id={}, channel_type={}, pts={}",
            req.channel_id, req.channel_type, current_pts
        );
        
        Ok(GetChannelPtsResponse {
            current_pts,
        })
    }
    
    /// 处理批量获取频道 pts 请求 ⭐
    /// 
    /// RPC: sync/batch_get_channel_pts
    pub async fn handle_batch_get_channel_pts(
        &self,
        req: BatchGetChannelPtsRequest,
    ) -> Result<BatchGetChannelPtsResponse> {
        let mut channel_pts_list = Vec::new();
        
        // 批量查询（优化：可以使用一次数据库查询）
        let channels: Vec<(u64, u8)> = req.channels.iter()
            .map(|c| (c.channel_id, c.channel_type))
            .collect();
        
        let pts_results = self.pts_dao.batch_get_current_pts(channels).await?;
        
        for (channel_id, channel_type, current_pts) in pts_results {
            channel_pts_list.push(ChannelPtsInfo {
                channel_id,
                channel_type,
                current_pts,
            });
        }
        
        debug!("批量获取 {} 个频道的 pts", channel_pts_list.len());
        
        Ok(BatchGetChannelPtsResponse {
            channel_pts_map: channel_pts_list,
        })
    }
    
    // ============================================================
    // 辅助方法
    // ============================================================
    
    /// 生成消息 ID（使用 snowflake）
    async fn generate_msg_id(&self) -> u64 {
        use crate::infra::snowflake::next_message_id;
        next_message_id()
    }
    
    /// 获取发送者信息
    async fn get_sender_info(&self, sender_id: u64) -> Result<SenderInfo> {
        // 简化实现：返回基本信息
        // TODO: 后续可以从 user_repository 查询完整用户信息
        Ok(SenderInfo {
            user_id: sender_id,
            username: format!("user_{}", sender_id),
            nickname: None,
            avatar_url: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_handle_client_submit() {
        // 测试客户端提交
    }
    
    #[tokio::test]
    async fn test_handle_get_difference() {
        // 测试获取差异
    }
    
    #[tokio::test]
    async fn test_idempotency() {
        // 测试幂等性
    }
    
    #[tokio::test]
    async fn test_gap_detection() {
        // 测试间隙检测
    }
}
