/// Phase 8: pts-Based 同步服务
/// 
/// 职责：
/// - 处理客户端提交命令（sync/submit）
/// - 提供差异拉取（sync/get_difference）
/// - 管理 pts 分配和间隙检测

use std::sync::Arc;
use tracing::{debug, error, info, warn};
use std::collections::HashMap;

use crate::error::Result;
use crate::model::pts::PtsGenerator;
use crate::infra::database::Database;
use crate::infra::redis::RedisClient;

// 重新导出协议类型
use privchat_protocol::rpc::sync::{
    ClientSubmitRequest, ClientSubmitResponse,
    GetDifferenceRequest, GetDifferenceResponse,
    GetChannelPtsRequest, GetChannelPtsResponse,
    BatchGetChannelPtsRequest, BatchGetChannelPtsResponse,
    ServerCommit, ServerDecision, ChannelPtsInfo, SenderInfo,
};

/// 同步服务
pub struct SyncService {
    /// pts 生成器（per-channel）
    pts_generator: Arc<PtsGenerator>,
    
    /// 数据库连接
    db: Arc<Database>,
    
    /// Redis 客户端
    redis: Arc<RedisClient>,
}

impl SyncService {
    /// 创建同步服务
    pub fn new(
        pts_generator: Arc<PtsGenerator>,
        db: Arc<Database>,
        redis: Arc<RedisClient>,
    ) -> Self {
        Self {
            pts_generator,
            db,
            redis,
        }
    }
    
    // ============================================================
    // RPC 处理方法
    // ============================================================
    
    /// 处理客户端提交命令
    /// 
    /// RPC: sync/submit
    pub async fn handle_client_submit(
        &self,
        req: ClientSubmitRequest,
        sender_id: u64, // 从 JWT token 中提取
    ) -> Result<ClientSubmitResponse> {
        info!(
            "收到客户端提交: local_message_id={}, channel_id={}, channel_type={}",
            req.local_message_id, req.channel_id, req.channel_type
        );
        
        // 1. 幂等性检查（local_message_id 去重）
        if let Some(existing) = self.check_duplicate(req.local_message_id).await? {
            info!("检测到重复提交: local_message_id={}", req.local_message_id);
            return Ok(existing);
        }
        
        // 2. 验证权限（TODO: 实现权限检查）
        // self.validate_permission(req.channel_id, sender_id).await?;
        
        // 3. 获取服务器当前 pts
        let server_pts = self.pts_generator.current_pts(req.channel_id).await;
        
        // 4. 检测间隙
        let has_gap = req.last_pts < server_pts.saturating_sub(1);
        
        if has_gap {
            warn!(
                "检测到 pts 间隙: channel_id={}, client_pts={}, server_pts={}",
                req.channel_id, req.last_pts, server_pts
            );
        }
        
        // 5. 分配新的 pts
        let new_pts = self.pts_generator.next_pts(req.channel_id).await;
        
        // 6. 生成 server_msg_id（TODO: 使用 snowflake）
        let server_msg_id = self.generate_msg_id().await;
        
        // 7. 构造 ServerCommit
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
        
        // 8. 保存到 Commit Log（数据库）
        self.save_commit(&commit).await?;
        
        // 9. 缓存到 Redis
        self.cache_commit(&commit).await?;
        
        // 10. Fan-out 给在线用户（TODO: 实现推送）
        // self.fanout_to_online_users(&commit).await?;
        
        // 11. 注册 local_message_id（防止重复）
        self.register_local_message_id(req.local_message_id, &commit).await?;
        
        // 12. 返回响应
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
            "✅ 命令已提交: local_message_id={}, pts={}, server_msg_id={}",
            req.local_message_id, new_pts, server_msg_id
        );
        
        Ok(response)
    }
    
    /// 处理获取差异请求
    /// 
    /// RPC: sync/get_difference
    pub async fn handle_get_difference(
        &self,
        req: GetDifferenceRequest,
    ) -> Result<GetDifferenceResponse> {
        let limit = req.limit.unwrap_or(100);
        
        info!(
            "收到差异拉取请求: channel_id={}, channel_type={}, last_pts={}, limit={}",
            req.channel_id, req.channel_type, req.last_pts, limit
        );
        
        // 1. 查询 Commit Log（pts > last_pts，按 pts 升序）
        let commits = self.query_commits(
            req.channel_id,
            req.channel_type,
            req.last_pts,
            limit,
        ).await?;
        
        // 2. 获取当前 pts
        let current_pts = self.pts_generator.current_pts(req.channel_id).await;
        
        // 3. 检查是否还有更多
        let has_more = if let Some(last_commit) = commits.last() {
            last_commit.pts < current_pts
        } else {
            false
        };
        
        info!(
            "✅ 差异拉取完成: 返回 {} 条 commits, has_more={}",
            commits.len(), has_more
        );
        
        Ok(GetDifferenceResponse {
            commits,
            current_pts,
            has_more,
        })
    }
    
    /// 处理获取频道 pts 请求
    /// 
    /// RPC: sync/get_channel_pts
    pub async fn handle_get_channel_pts(
        &self,
        req: GetChannelPtsRequest,
    ) -> Result<GetChannelPtsResponse> {
        let current_pts = self.pts_generator.current_pts(req.channel_id).await;
        
        debug!(
            "获取频道 pts: channel_id={}, channel_type={}, pts={}",
            req.channel_id, req.channel_type, current_pts
        );
        
        Ok(GetChannelPtsResponse {
            current_pts,
        })
    }
    
    /// 处理批量获取频道 pts 请求
    /// 
    /// RPC: sync/batch_get_channel_pts
    pub async fn handle_batch_get_channel_pts(
        &self,
        req: BatchGetChannelPtsRequest,
    ) -> Result<BatchGetChannelPtsResponse> {
        let mut channel_pts_list = Vec::new();
        
        for channel in &req.channels {
            let current_pts = self.pts_generator
                .current_pts(channel.channel_id)
                .await;
            
            channel_pts_list.push(ChannelPtsInfo {
                channel_id: channel.channel_id,
                channel_type: channel.channel_type,
                current_pts,
            });
        }
        
        debug!("批量获取 {} 个频道的 pts", channel_pts_list.len());
        
        Ok(BatchGetChannelPtsResponse {
            channel_pts_map: channel_pts_list,
        })
    }
    
    // ============================================================
    // 私有辅助方法
    // ============================================================
    
    /// 检查是否重复提交
    async fn check_duplicate(&self, local_message_id: u64) -> Result<Option<ClientSubmitResponse>> {
        // TODO: 从 privchat_client_msg_registry 表查询
        // 如果存在，返回之前的响应
        Ok(None)
    }
    
    /// 保存 Commit 到数据库
    async fn save_commit(&self, commit: &ServerCommit) -> Result<()> {
        // TODO: 保存到 privchat_commit_log 表
        debug!("保存 Commit: pts={}, server_msg_id={}", commit.pts, commit.server_msg_id);
        Ok(())
    }
    
    /// 缓存 Commit 到 Redis
    async fn cache_commit(&self, commit: &ServerCommit) -> Result<()> {
        // TODO: 保存到 Redis Sorted Set
        // Key: commit_log:{channel_id}:{channel_type}
        // Score: pts
        // Value: JSON(commit)
        Ok(())
    }
    
    /// 查询 Commits
    async fn query_commits(
        &self,
        channel_id: u64,
        channel_type: u8,
        last_pts: u64,
        limit: u32,
    ) -> Result<Vec<ServerCommit>> {
        // TODO: 先查 Redis，再查数据库
        debug!(
            "查询 Commits: channel_id={}, channel_type={}, last_pts={}, limit={}",
            channel_id, channel_type, last_pts, limit
        );
        
        // 暂时返回空列表
        Ok(Vec::new())
    }
    
    /// 生成消息 ID
    async fn generate_msg_id(&self) -> u64 {
        // TODO: 使用 snowflake 生成
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        now
    }
    
    /// 获取发送者信息
    async fn get_sender_info(&self, sender_id: u64) -> Result<SenderInfo> {
        // TODO: 从数据库查询用户信息
        Ok(SenderInfo {
            user_id: sender_id,
            username: format!("user_{}", sender_id),
            nickname: None,
            avatar_url: None,
        })
    }
    
    /// 注册 local_message_id
    async fn register_local_message_id(&self, local_message_id: u64, commit: &ServerCommit) -> Result<()> {
        // TODO: 保存到 privchat_client_msg_registry 表
        debug!("注册 local_message_id: {}", local_message_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_pts_generator_per_channel() {
        let generator = PtsGenerator::new();
        
        // 频道 1 的 pts
        let pts1_ch1 = generator.next_pts(1001).await;
        assert_eq!(pts1_ch1, 1);
        
        let pts2_ch1 = generator.next_pts(1001).await;
        assert_eq!(pts2_ch1, 2);
        
        // 频道 2 的 pts（独立递增）
        let pts1_ch2 = generator.next_pts(1002).await;
        assert_eq!(pts1_ch2, 1); // 从 1 开始，不是 3
        
        // 群聊频道的 pts（独立递增）
        let pts1_group = generator.next_pts(2001).await;
        assert_eq!(pts1_group, 1);
        
        // 验证独立性
        let current_ch1 = generator.current_pts(1001).await;
        assert_eq!(current_ch1, 2);
        
        let current_ch2 = generator.current_pts(1002).await;
        assert_eq!(current_ch2, 1);
        
        let current_group = generator.current_pts(2001).await;
        assert_eq!(current_group, 1);
        
        println!("✅ per-channel pts 测试通过");
    }
}
