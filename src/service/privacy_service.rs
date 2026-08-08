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

use crate::error::{Result, ServerError};
use crate::infra::CacheManager;
use crate::model::privacy::{UserDetailSource, UserPrivacySettings};
use crate::service::ChannelService;
use crate::service::FriendService;
use chrono::Utc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

/// detail 访问裁决(PROFILE_VISIBILITY §2.5):来源真伪已通过,
/// can_add_friend/username_unlocked 是 detail 时刻的判定快照。
#[derive(Debug, Clone)]
pub struct DetailAccessVerdict {
    pub is_friend: bool,
    pub can_add_friend: bool,
    pub deny_reason: Option<&'static str>,
    pub username_unlocked: bool,
}

impl DetailAccessVerdict {
    fn viewable_and_addable() -> Self {
        Self {
            is_friend: false,
            can_add_friend: true,
            deny_reason: None,
            username_unlocked: false,
        }
    }

    fn view_only(reason: &'static str) -> Self {
        Self {
            is_friend: false,
            can_add_friend: false,
            deny_reason: Some(reason),
            username_unlocked: false,
        }
    }
}

/// 隐私和权限验证服务
/// 割接期停写开关：`PRIVCHAT_PRIVACY_WRITES_FROZEN=1`。
///
/// 存在的理由是 `PRIVACY_SETTINGS_CUTOVER_SOP` 路线 B 需要一个「旧服务不再产生
/// 只落 Redis 的新写入」的窗口。没有这个开关，那条流程就只是文档里的一句话——
/// 导出快照之后到新版本上线之前的用户修改照样会丢。
///
/// 只读一次：割接窗口靠重启进出，不做热切换（热切换会带来「一半请求写、一半请求拒」
/// 的中间态，正是这个开关要消灭的东西）。
fn privacy_writes_frozen() -> bool {
    static FROZEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FROZEN.get_or_init(|| {
        matches!(
            std::env::var("PRIVCHAT_PRIVACY_WRITES_FROZEN").as_deref(),
            Ok("1") | Ok("true")
        )
    })
}

pub struct PrivacyService {
    cache_manager: Arc<CacheManager>,
    channel_service: Arc<ChannelService>,
    friend_service: Arc<FriendService>,
    /// 平台级「按用户名搜索」开关的内存镜像(真源 privchat_platform_settings)。
    platform_username_searchable: AtomicBool,
    /// 0=未加载 1=已加载(懒加载一次,admin 更新时同步刷新)
    platform_loaded: AtomicU8,
}

impl PrivacyService {
    pub fn new(
        cache_manager: Arc<CacheManager>,
        channel_service: Arc<ChannelService>,
        friend_service: Arc<FriendService>,
    ) -> Self {
        Self {
            cache_manager,
            channel_service,
            friend_service,
            platform_username_searchable: AtomicBool::new(true),
            platform_loaded: AtomicU8::new(0),
        }
    }

    const PLATFORM_KEY_USERNAME_SEARCHABLE: &'static str = "privacy.username_searchable";

    /// 平台级:是否开放按用户名搜索(D4 顶层;默认 true)。懒加载 DB 一次,
    /// admin 更新时写库并同步内存,多实例部署下其它实例最迟重启后收敛
    /// (单实例 fail-closed 部署模型下即时生效)。
    pub async fn platform_username_searchable(&self) -> bool {
        if self.platform_loaded.load(Ordering::Acquire) == 0 {
            let loaded: Option<bool> = sqlx::query_scalar(
                "SELECT (value #>> '{}')::boolean FROM privchat_platform_settings WHERE key = $1",
            )
            .bind(Self::PLATFORM_KEY_USERNAME_SEARCHABLE)
            .fetch_optional(self.channel_service.pool())
            .await
            .ok()
            .flatten();
            if let Some(v) = loaded {
                self.platform_username_searchable
                    .store(v, Ordering::Release);
            }
            self.platform_loaded.store(1, Ordering::Release);
        }
        self.platform_username_searchable.load(Ordering::Acquire)
    }

    /// admin:更新平台级用户名搜索开关(写库 + 刷内存)。
    pub async fn set_platform_username_searchable(&self, enabled: bool) -> Result<()> {
        sqlx::query(
            "INSERT INTO privchat_platform_settings (key, value, updated_at)
             VALUES ($1, to_jsonb($2::boolean), $3)
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at",
        )
        .bind(Self::PLATFORM_KEY_USERNAME_SEARCHABLE)
        .bind(enabled)
        .bind(Utc::now().timestamp_millis())
        .execute(self.channel_service.pool())
        .await
        .map_err(|e| ServerError::Database(format!("persist platform privacy setting: {e}")))?;
        self.platform_username_searchable
            .store(enabled, Ordering::Release);
        self.platform_loaded.store(1, Ordering::Release);
        Ok(())
    }

    /// 评估查看用户资料的权限(PROFILE_VISIBILITY §2.5)。
    ///
    /// 两层语义分离:
    ///   - **来源真伪**不通过 → Err(拒绝整个 detail 请求);
    ///   - **加好友权限**(群策略/个人「添加我的方式」开关)不通过 →
    ///     仍可查看公开投影,只是 verdict.can_add_friend=false + deny_reason。
    ///
    /// username_unlocked 仅在 by_username 精确搜索来源(D1)或好友关系下为 true。
    pub async fn evaluate_detail_access(
        &self,
        searcher_id: u64,
        target_id: u64,
        source: UserDetailSource,
    ) -> Result<DetailAccessVerdict> {
        // 本人查本人:无需任何来源。此前会走到下面的好友判定,得出「你不是你自己的好友」
        // 而整体拒绝(生产实测 876 次/天),自查必须最先放行。
        if searcher_id == target_id {
            return Ok(DetailAccessVerdict {
                is_friend: false,
                can_add_friend: false,
                deny_reason: Some("self"),
                username_unlocked: true,
            });
        }

        // 好友是最高权限:来源无需再验,username 可见,不能重复添加。
        //
        // 必须用 try_is_friend:`is_friend` 内部 `unwrap_or(false)` 会把数据库错误吞成
        // 「不是好友」,导致 DB 抖动期间好友被降级按来源校验(可能直接被拒)。权限判定
        // 不能建立在「查不到就当没有」上。
        if self
            .friend_service
            .try_is_friend(searcher_id, target_id)
            .await?
        {
            tracing::debug!("✅ 好友关系验证通过: {} -> {}", searcher_id, target_id);
            return Ok(DetailAccessVerdict {
                is_friend: true,
                can_add_friend: false,
                deny_reason: Some("already_friend"),
                username_unlocked: true,
            });
        }

        match source {
            // 客户端声称 self 但 searcher != target：来源不实（上面的 self 短路没命中）。
            UserDetailSource::SelfProfile => Err(ServerError::Forbidden(
                "Self source claimed but searcher is not the target".to_string(),
            )),
            UserDetailSource::Search { search_session_id } => {
                self.evaluate_search_source(searcher_id, target_id, search_session_id)
                    .await
            }
            UserDetailSource::Group { group_id } => {
                self.evaluate_group_source(searcher_id, target_id, group_id)
                    .await
            }
            UserDetailSource::Friend { friend_id: _ } => {
                // 声称好友来源但上面已判非好友 → 来源不实。
                Err(ServerError::Forbidden(
                    "Friend source claimed but users are not friends".to_string(),
                ))
            }
            UserDetailSource::CardShare { share_id } => {
                self.validate_card_share_source(searcher_id, target_id, share_id)
                    .await?;
                let privacy = self.get_or_create_privacy_settings(target_id).await?;
                Ok(if privacy.allow_add_by_card {
                    DetailAccessVerdict::viewable_and_addable()
                } else {
                    DetailAccessVerdict::view_only("personal_privacy")
                })
            }
            UserDetailSource::Conversation { channel_id } => {
                // 来源真伪(PROFILE_VISIBILITY §2.5 表):**viewer ∈ channel ∧ target ∈ channel**。
                // 此前这里完全没有成员校验,任何已认证用户传一个任意 channel_id 就能拿到
                // 任意人的公开投影且 can_add_friend=true。
                //
                // 群会话直接**复用 group 来源的同一套判定**(evaluate_group_source):
                // 双向成员校验 + 群策略 + 群主/管理员豁免 + 个人 allow_add_by_group。
                // 两条路径共用一个函数,避免「同一个群、点进资料的入口不同结论不同」的漂移。
                let channel = self.channel_service.get_channel(&channel_id).await?;
                if channel.channel_type == crate::model::channel::ChannelType::Group {
                    return self
                        .evaluate_group_source(searcher_id, target_id, channel_id)
                        .await;
                }

                // DM:双方即成员,校验后放行(DM 本就互通,不套群策略)。
                if !channel.members.contains_key(&searcher_id) {
                    return Err(ServerError::Forbidden(format!(
                        "User {} is not a member of channel {}",
                        searcher_id, channel_id
                    )));
                }
                if !channel.members.contains_key(&target_id) {
                    return Err(ServerError::Forbidden(format!(
                        "User {} is not a member of channel {}",
                        target_id, channel_id
                    )));
                }
                Ok(DetailAccessVerdict::viewable_and_addable())
            }
        }
    }

    /// 兼容入口:仅裁决"能否查看"(来源真伪)。旧调用点使用。
    pub async fn validate_detail_access(
        &self,
        searcher_id: u64,
        target_id: u64,
        source: UserDetailSource,
    ) -> Result<()> {
        self.evaluate_detail_access(searcher_id, target_id, source)
            .await
            .map(|_| ())
    }

    /// 评估搜索来源:真伪(归属/目标/过期)Err;命中方式映射个人搜索开关;
    /// by_username 命中解锁 username 回显。
    async fn evaluate_search_source(
        &self,
        searcher_id: u64,
        target_id: u64,
        search_session_id: u64,
    ) -> Result<DetailAccessVerdict> {
        let record = self
            .cache_manager
            .get_search_record(search_session_id)
            .await?
            .ok_or_else(|| {
                ServerError::NotFound(format!("Search record not found: {}", search_session_id))
            })?;

        if record.searcher_id != searcher_id {
            return Err(ServerError::Forbidden(
                "Search record does not belong to this user".to_string(),
            ));
        }
        if record.target_id != target_id {
            return Err(ServerError::Forbidden(
                "Search record target does not match".to_string(),
            ));
        }
        if record.is_expired() {
            return Err(ServerError::Forbidden(
                "Search record has expired".to_string(),
            ));
        }

        let privacy = self.get_or_create_privacy_settings(target_id).await?;

        // 按真实命中方式查对应开关;老记录无 hit_by 时退回"任一允许"旧语义。
        let allowed = match record.hit_by {
            Some(t) => privacy.allows_search(t),
            None => {
                privacy.allow_search_by_username
                    || privacy.allow_search_by_phone
                    || privacy.allow_search_by_email
            }
        };
        if !allowed {
            return Err(ServerError::Forbidden(
                "User does not allow being searched".to_string(),
            ));
        }

        Ok(DetailAccessVerdict {
            is_friend: false,
            can_add_friend: true,
            deny_reason: None,
            username_unlocked: matches!(
                record.hit_by,
                Some(crate::model::privacy::SearchType::Username)
            ),
        })
    }

    /// 评估群来源:双方成员身份是真伪校验(Err);群策略与个人开关只影响
    /// can_add_friend(查看保留公开投影,微信同款)。
    async fn evaluate_group_source(
        &self,
        searcher_id: u64,
        target_id: u64,
        group_id: u64,
    ) -> Result<DetailAccessVerdict> {
        let channel = self.channel_service.get_channel(&group_id).await?;
        if !channel.members.contains_key(&searcher_id) {
            return Err(ServerError::Forbidden(format!(
                "User {} is not a member of group {}",
                searcher_id, group_id
            )));
        }
        if !channel.members.contains_key(&target_id) {
            return Err(ServerError::Forbidden(format!(
                "User {} is not a member of group {}",
                target_id, group_id
            )));
        }

        // 群策略:allow_member_add_friend=false 时,群主/管理员(任一方)豁免。
        let allow_member_add_friend = self
            .channel_service
            .get_group_policy(group_id)
            .await
            .ok()
            .flatten()
            .map(|p| p.allow_member_add_friend)
            .unwrap_or(true);
        if !allow_member_add_friend {
            use crate::model::channel::MemberRole;
            let is_privileged = |uid: &u64| {
                channel
                    .members
                    .get(uid)
                    .map(|m| matches!(m.role, MemberRole::Owner | MemberRole::Admin))
                    .unwrap_or(false)
            };
            if !is_privileged(&searcher_id) && !is_privileged(&target_id) {
                return Ok(DetailAccessVerdict::view_only("group_policy"));
            }
        }

        // 个人「添加我的方式」:允许通过群聊添加我(§2.5,20312)。
        let privacy = self.get_or_create_privacy_settings(target_id).await?;
        if !privacy.allow_add_by_group {
            return Ok(DetailAccessVerdict::view_only("personal_privacy"));
        }

        Ok(DetailAccessVerdict::viewable_and_addable())
    }

    /// 验证名片分享来源
    async fn validate_card_share_source(
        &self,
        searcher_id: u64,
        target_id: u64,
        share_id: u64,
    ) -> Result<()> {
        // 1. 获取分享记录
        let record = self
            .cache_manager
            .get_card_share(share_id)
            .await?
            .ok_or_else(|| {
                ServerError::NotFound(format!("Card share record not found: {}", share_id))
            })?;

        // 2. 验证接收者
        if record.receiver_id != searcher_id {
            return Err(ServerError::Forbidden(
                "Card share record does not belong to this user".to_string(),
            ));
        }

        // 3. 验证目标用户
        if record.target_user_id != target_id {
            return Err(ServerError::Forbidden(
                "Card share record target does not match".to_string(),
            ));
        }

        // 4. 验证是否已被使用
        if record.used {
            return Err(ServerError::Forbidden(
                "Card share record has already been used".to_string(),
            ));
        }

        Ok(())
    }

    /// 获取或创建默认隐私设置
    /// 读用户隐私设置。**直读数据库，不走任何缓存。**
    ///
    /// 🔴 为什么不缓存：这是发送授权的判据。缓存一旦引入，就要回答
    /// 「另一个实例什么时候看到新值」——而删共享 key 管不住别的实例的进程内
    /// 缓存，做对需要 Pub/Sub 或版本号那一整套。为一次数据库往返引入分布式
    /// 一致性机制不划算；而做不对的后果是用户关掉「接收非好友消息」之后，
    /// 在另一台机器上继续收到陌生人消息。
    ///
    /// 要加缓存的前提是先有失效广播，并且有一个**用真 Redis 双实例**跑的测试
    /// （用 `CacheConfig::default()` 的测试证明不了任何跨实例行为——那里
    /// `redis=None`，根本没有共享缓存）。
    pub async fn get_or_create_privacy_settings(
        &self,
        user_id: u64,
    ) -> Result<UserPrivacySettings> {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT privacy_settings FROM privchat_users WHERE user_id = $1")
                .bind(user_id as i64)
                .fetch_optional(self.channel_service.pool())
                .await
                .map_err(|e| ServerError::Database(format!("查询隐私设置失败: {e}")))?;

        Self::settings_from_stored(user_id, row.map(|(value,)| value))
    }

    /// 把 DB 里存的（部分字段）JSON 解析成完整设置。
    ///
    /// 缺字段是正常的增量存储；**已知字段类型不对是脏数据，必须报错**——
    /// 回落默认等于把用户的限制悄悄关掉（默认允许非好友消息）。
    /// 未知字段忽略：滚动升级期间新版本会写老版本不认识的键，
    /// 拒绝未知字段会让老实例把整行判脏、进而拒发消息。
    fn settings_from_stored(
        user_id: u64,
        stored: Option<serde_json::Value>,
    ) -> Result<UserPrivacySettings> {
        let mut settings = UserPrivacySettings::new(user_id);
        if let Some(value) = stored {
            if !value.is_null() {
                let parsed: StoredPrivacySettings =
                    serde_json::from_value(value).map_err(|e| {
                        ServerError::Database(format!(
                            "用户 {user_id} 的隐私设置无法解析（脏数据，不按默认放行）: {e}"
                        ))
                    })?;
                parsed.apply_to(&mut settings);
            }
        }
        Ok(settings)
    }

    /// 更新隐私设置
    /// 更新隐私设置。**事务内 patch → 解析 → 提交。**
    ///
    /// 🔴 解析放在提交之前：写进去的东西如果解析不出来，事务直接回滚，
    /// 不会留下一行「已落库但没人能读」的脏数据。
    ///
    /// 也不再先读一遍旧值：那一次读的结果随后会被整份覆盖（编译器都报了
    /// unused assignment），白多一次往返；更糟的是已知字段损坏时，
    /// 读会先报错，于是用户**连改回正确值都做不到**。
    pub async fn update_privacy_settings(
        &self,
        user_id: u64,
        updates: PrivacySettingsUpdate,
    ) -> Result<UserPrivacySettings> {
        // 割接停写窗口（PRIVACY_SETTINGS_CUTOVER_SOP 路线 B 的 B1）。
        //
        // 🔴 必须是**可重试**错误码：返回终局失败会让客户端把这次修改当作被拒绝而丢弃，
        // 窗口期的用户操作就真没了——那跟不做停写一样糟。
        if privacy_writes_frozen() {
            return Err(ServerError::ServiceUnavailable(
                "隐私设置正在维护，请稍后重试".to_string(),
            ));
        }

        let patch = updates.to_patch_json();

        let mut tx = self
            .channel_service
            .pool()
            .begin()
            .await
            .map_err(|e| ServerError::Database(format!("开启隐私设置事务失败: {e}")))?;

        let updated: Option<(serde_json::Value,)> = sqlx::query_as(
            "UPDATE privchat_users \
             SET privacy_settings = COALESCE(privacy_settings, '{}'::jsonb) || $2::jsonb, \
                 updated_at = now_millis() \
             WHERE user_id = $1 \
             RETURNING privacy_settings",
        )
        .bind(user_id as i64)
        .bind(&patch)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("写入隐私设置失败: {e}")))?;

        let Some((stored,)) = updated else {
            tx.rollback().await.ok();
            return Err(ServerError::NotFound(format!("用户 {user_id} 不存在")));
        };

        // 解析失败就回滚：宁可这次更新失败，也不留一行读不出来的设置。
        let mut settings = match Self::settings_from_stored(user_id, Some(stored)) {
            Ok(settings) => settings,
            Err(e) => {
                tx.rollback().await.ok();
                return Err(e);
            }
        };
        tx.commit()
            .await
            .map_err(|e| ServerError::Database(format!("提交隐私设置失败: {e}")))?;

        settings.updated_at = Utc::now();
        Ok(settings)
    }
}

/// DB 里存着的隐私设置（**部分字段**）。
///
/// 单独一个 DTO 而不是直接反序列化 [`UserPrivacySettings`]：存的是增量，
/// 缺字段属正常，字段类型不对属脏数据——两者必须区分开，
/// 否则「缺字段」和「坏数据」都会走到同一个「回落默认」，把限制策略关掉。
/// 🔴 **不加 `deny_unknown_fields`**：滚动升级期间新版本会写入老版本不认识的
/// 字段，拒绝未知字段等于让老实例把整行判成脏数据、进而拒发消息。
/// 边界是「未知字段忽略，已知字段类型错误拒绝」。
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct StoredPrivacySettings {
    allow_add_by_group: Option<bool>,
    allow_add_by_card: Option<bool>,
    allow_search_by_phone: Option<bool>,
    allow_search_by_username: Option<bool>,
    allow_search_by_email: Option<bool>,
    allow_search_by_qrcode: Option<bool>,
    allow_view_by_non_friend: Option<bool>,
    allow_receive_message_from_non_friend: Option<bool>,
    #[serde(default)]
    user_id: Option<u64>,
    #[serde(default)]
    created_at: Option<serde_json::Value>,
    #[serde(default)]
    updated_at: Option<serde_json::Value>,
}

impl StoredPrivacySettings {
    fn apply_to(self, settings: &mut UserPrivacySettings) {
        macro_rules! apply {
            ($($field:ident),+ $(,)?) => {
                $(if let Some(value) = self.$field { settings.$field = value; })+
            };
        }
        apply!(
            allow_add_by_group,
            allow_add_by_card,
            allow_search_by_phone,
            allow_search_by_username,
            allow_search_by_email,
            allow_search_by_qrcode,
            allow_view_by_non_friend,
            allow_receive_message_from_non_friend,
        );
    }
}

/// 隐私设置更新（部分更新）
#[derive(Debug, Clone, Default)]
pub struct PrivacySettingsUpdate {
    pub allow_add_by_group: Option<bool>,
    pub allow_search_by_phone: Option<bool>,
    pub allow_search_by_username: Option<bool>,
    pub allow_search_by_email: Option<bool>,
    pub allow_search_by_qrcode: Option<bool>,
    pub allow_view_by_non_friend: Option<bool>,
    pub allow_receive_message_from_non_friend: Option<bool>,
}

impl PrivacySettingsUpdate {
    /// 只把**本次真正要改的字段**变成 JSON patch。
    ///
    /// 没设的字段不出现在 patch 里，因此 `privacy_settings || patch` 不会碰它们——
    /// 这正是「两台设备同时改不同字段不会互相覆盖」的来源。
    fn to_patch_json(&self) -> serde_json::Value {
        let mut patch = serde_json::Map::new();
        macro_rules! put {
            ($($field:ident),+ $(,)?) => {
                $(if let Some(value) = self.$field {
                    patch.insert(stringify!($field).to_string(), serde_json::Value::Bool(value));
                })+
            };
        }
        put!(
            allow_add_by_group,
            allow_search_by_phone,
            allow_search_by_username,
            allow_search_by_email,
            allow_search_by_qrcode,
            allow_view_by_non_friend,
            allow_receive_message_from_non_friend,
        );
        serde_json::Value::Object(patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CacheConfig;
    use crate::model::channel::{ChannelType, CreateChannelRequest};
    use crate::repository::PgChannelRepository;
    use crate::rpc::qr::generate_qr_key;
    use sqlx::postgres::PgPoolOptions;

    /// A1 验收门禁（PROFILE_VISIBILITY §2.5）。
    ///
    /// 背景：`conversation` 来源过去**完全没有成员校验**，任何已认证用户传一个任意
    /// channel_id 就能拿到任意人的公开投影；而 `group` 来源一直是双向校验的。
    /// 另外 self 查询会走到好友判定得出「你不是你自己的好友」而整体被拒。
    /// 安全门禁不能靠「没数据库就跳过」显示绿色:CI 里设 `PRIVCHAT_REQUIRE_DB=1`,
    /// 数据库不可用时**直接失败**,而不是静默 skip 出一个假绿。
    async fn open_service() -> Option<PrivacyService> {
        let require_db = std::env::var("PRIVCHAT_REQUIRE_DB").ok().as_deref() == Some("1");
        let url = match std::env::var("PRIVCHAT_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
        {
            Ok(u) => u,
            Err(_) if require_db => {
                panic!("PRIVCHAT_REQUIRE_DB=1 但没有配置 DATABASE_URL:权限门禁测试必须真跑")
            }
            Err(_) => return None,
        };
        let pool = match PgPoolOptions::new().max_connections(4).connect(&url).await {
            Ok(p) => Arc::new(p),
            Err(e) if require_db => panic!("PRIVCHAT_REQUIRE_DB=1 但连不上数据库: {e}"),
            Err(_) => return None,
        };
        let cache = Arc::new(CacheManager::new(CacheConfig::default()).await.ok()?);
        let channel_service = Arc::new(ChannelService::new_with_repository(Arc::new(
            PgChannelRepository::new(pool.clone()),
        )));
        let friend_service = Arc::new(FriendService::new(pool));
        Some(PrivacyService::new(cache, channel_service, friend_service))
    }

    /// 指向不存在的数据库：任何 DB 访问都会失败。用于证明权限判定**不会把数据库
    /// 故障降级成「不是好友 / 没有权限」**。
    async fn broken_service() -> PrivacyService {
        let pool = Arc::new(
            PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(std::time::Duration::from_millis(300))
                .connect_lazy("postgres://nobody:nobody@127.0.0.1:1/nonexistent")
                .expect("lazy pool"),
        );
        let cache = Arc::new(
            CacheManager::new(CacheConfig::default())
                .await
                .expect("cache"),
        );
        let channel_service = Arc::new(ChannelService::new_with_repository(Arc::new(
            PgChannelRepository::new(pool.clone()),
        )));
        let friend_service = Arc::new(FriendService::new(pool));
        PrivacyService::new(cache, channel_service, friend_service)
    }

    async fn ensure_user(svc: &PrivacyService, user_id: u64, username: &str) {
        let qr_key = generate_qr_key();
        let _ = sqlx::query(
            r#"
            INSERT INTO privchat_users (user_id, username, display_name, qr_key)
            VALUES ($1, $2, $2, $3)
            ON CONFLICT (user_id) DO UPDATE SET username = EXCLUDED.username
            "#,
        )
        .bind(user_id as i64)
        .bind(username)
        .bind(&qr_key)
        .execute(svc.channel_service.pool())
        .await
        .expect("ensure user");
    }

    async fn cleanup(svc: &PrivacyService, channel_id: u64, users: &[u64]) {
        let _ = sqlx::query("DELETE FROM privchat_channel_participants WHERE channel_id = $1")
            .bind(channel_id as i64)
            .execute(svc.channel_service.pool())
            .await;
        let _ = sqlx::query("DELETE FROM privchat_channels WHERE channel_id = $1")
            .bind(channel_id as i64)
            .execute(svc.channel_service.pool())
            .await;
        for u in users {
            let _ = sqlx::query("DELETE FROM privchat_users WHERE user_id = $1")
                .bind(*u as i64)
                .execute(svc.channel_service.pool())
                .await;
        }
    }

    async fn create_dm(svc: &PrivacyService, a: u64, b: u64) -> u64 {
        let resp = svc
            .channel_service
            .create_channel(
                a,
                CreateChannelRequest {
                    channel_type: ChannelType::Direct,
                    name: None,
                    description: None,
                    member_ids: vec![b],
                    is_public: None,
                    max_members: None,
                },
            )
            .await
            .expect("create dm");
        assert!(resp.success, "{:?}", resp.error);
        resp.channel.id
    }

    #[tokio::test]
    async fn self_lookup_is_allowed_without_any_source() {
        let Some(svc) = open_service().await else {
            eprintln!("skip: DATABASE_URL not configured");
            return;
        };
        let uid = 940_101_u64;
        ensure_user(&svc, uid, "privacy_self_940101").await;

        // 传一个必然通不过真伪校验的来源，仍应因为「是本人」而放行。
        let verdict = svc
            .evaluate_detail_access(uid, uid, UserDetailSource::Friend { friend_id: Some(uid) })
            .await
            .expect("self lookup must not be denied");
        assert!(verdict.username_unlocked, "本人可见自己的 username");
        assert!(!verdict.can_add_friend, "不能加自己为好友");

        cleanup(&svc, 0, &[uid]).await;
    }

    /// self 来源必须端到端可用:协议枚举 → RPC parser → 权限判定。
    /// 第一版只在 service 层放行了本人,却给客户端造了一个协议根本不认的 "self" 字符串,
    /// 请求会在 parser 就被打回 —— service 层单测绿不代表 RPC 通。
    #[tokio::test]
    async fn self_source_parses_and_only_works_for_the_owner() {
        use privchat_protocol::rpc::account::user::DetailSourceType;
        assert_eq!(
            DetailSourceType::from_str("self"),
            Some(DetailSourceType::SelfProfile),
            "协议必须认识 self 来源"
        );
        assert_eq!(DetailSourceType::SelfProfile.as_str(), "self");

        let Some(svc) = open_service().await else {
            eprintln!("skip: DATABASE_URL not configured");
            return;
        };
        let me = 940_601_u64;
        let other = 940_602_u64;
        ensure_user(&svc, me, "privacy_self_940601").await;
        ensure_user(&svc, other, "privacy_self_940602").await;

        svc.evaluate_detail_access(me, me, UserDetailSource::SelfProfile)
            .await
            .expect("本人用 self 来源必须放行");

        // 冒用 self 去看别人:来源不实,拒绝。
        let err = svc
            .evaluate_detail_access(me, other, UserDetailSource::SelfProfile)
            .await
            .expect_err("self 来源不能用来看别人");
        assert!(matches!(err, ServerError::Forbidden(_)), "{err:?}");

        cleanup(&svc, 0, &[me, other]).await;
    }

    #[tokio::test]
    async fn conversation_source_rejects_unknown_channel() {
        let Some(svc) = open_service().await else {
            eprintln!("skip: DATABASE_URL not configured");
            return;
        };
        let a = 940_201_u64;
        let b = 940_202_u64;
        ensure_user(&svc, a, "privacy_conv_940201").await;
        ensure_user(&svc, b, "privacy_conv_940202").await;

        let err = svc
            .evaluate_detail_access(
                a,
                b,
                UserDetailSource::Conversation {
                    channel_id: 949_999_999,
                },
            )
            .await
            .expect_err("伪造的 channel 必须被拒");
        assert!(
            matches!(err, ServerError::NotFound(_) | ServerError::Forbidden(_)),
            "unexpected error: {err:?}"
        );

        cleanup(&svc, 0, &[a, b]).await;
    }

    #[tokio::test]
    async fn conversation_source_requires_both_sides_to_be_members() {
        let Some(svc) = open_service().await else {
            eprintln!("skip: DATABASE_URL not configured");
            return;
        };
        let a = 940_301_u64;
        let b = 940_302_u64;
        let outsider = 940_303_u64;
        ensure_user(&svc, a, "privacy_conv_940301").await;
        ensure_user(&svc, b, "privacy_conv_940302").await;
        ensure_user(&svc, outsider, "privacy_conv_940303").await;
        let channel_id = create_dm(&svc, a, b).await;

        // viewer 在频道内、target 不在 → 拒绝
        let err = svc
            .evaluate_detail_access(a, outsider, UserDetailSource::Conversation { channel_id })
            .await
            .expect_err("target 不在频道必须被拒");
        assert!(matches!(err, ServerError::Forbidden(_)), "{err:?}");

        // target 在频道内、viewer 不在 → 拒绝（此前这条完全没人拦）
        let err = svc
            .evaluate_detail_access(outsider, b, UserDetailSource::Conversation { channel_id })
            .await
            .expect_err("viewer 不在频道必须被拒");
        assert!(matches!(err, ServerError::Forbidden(_)), "{err:?}");

        // 双方都是 DM 成员 → 放行
        let verdict = svc
            .evaluate_detail_access(a, b, UserDetailSource::Conversation { channel_id })
            .await
            .expect("DM 双方互查应放行");
        assert!(!verdict.is_friend, "DM 成员不等于好友");

        cleanup(&svc, channel_id, &[a, b, outsider]).await;
    }

    #[tokio::test]
    async fn database_failure_is_not_downgraded_to_not_friend() {
        let svc = broken_service().await;
        // 好友表查不动时，绝不能得出「不是好友 → 来源不实 → Forbidden」这种把
        // 基础设施故障说成权限问题的结论。
        let err = svc
            .evaluate_detail_access(940_401, 940_402, UserDetailSource::Friend { friend_id: None })
            .await
            .expect_err("DB 故障必须上抛");
        assert!(
            matches!(err, ServerError::Database(_)),
            "expected Database error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn conversation_membership_pass_still_applies_group_and_privacy_gates() {
        let Some(svc) = open_service().await else {
            eprintln!("skip: DATABASE_URL not configured");
            return;
        };
        let owner = 940_501_u64;
        let member = 940_502_u64;
        ensure_user(&svc, owner, "privacy_conv_940501").await;
        ensure_user(&svc, member, "privacy_conv_940502").await;

        let resp = svc
            .channel_service
            .create_channel(
                owner,
                CreateChannelRequest {
                    channel_type: ChannelType::Group,
                    name: Some("privacy-conv-group".to_string()),
                    description: None,
                    member_ids: vec![member],
                    is_public: None,
                    max_members: None,
                },
            )
            .await
            .expect("create group");
        assert!(resp.success, "{:?}", resp.error);
        let channel_id = resp.channel.id;

        // 群成员互查:来源成立 → 可看公开投影。
        let verdict = svc
            .evaluate_detail_access(owner, member, UserDetailSource::Conversation { channel_id })
            .await
            .expect("群成员互查应放行");
        assert!(verdict.can_add_friend, "默认群策略下允许加好友");

        // 关掉「群成员互加好友」→ 仍可看资料,加好友能力按**群来源同一套规则**判定。
        //
        // ⚠️ 这里我第一版写错过:断言「owner 查 member 也不能加好友」。而
        // evaluate_group_source 明确规定群主/管理员任一方**豁免**该策略,
        // conversation 来源既然复用同一函数,就必须得出同样结论,否则同一个群
        // 从会话进和从成员列表进会给出两种答案。
        sqlx::query("UPDATE privchat_groups SET allow_member_add_friend = false WHERE group_id = $1")
            .bind(channel_id as i64)
            .execute(svc.channel_service.pool())
            .await
            .expect("tighten group policy");

        let owner_view = svc
            .evaluate_detail_access(owner, member, UserDetailSource::Conversation { channel_id })
            .await
            .expect("收紧策略后仍可查看公开投影");
        assert!(
            owner_view.can_add_friend,
            "群主豁免 allow_member_add_friend(与 group 来源一致)",
        );

        // 两个普通成员之间才真正被策略挡住。
        let plain = 940_503_u64;
        ensure_user(&svc, plain, "privacy_conv_940503").await;
        svc.channel_service
            .add_member_to_group(channel_id, plain)
            .await
            .expect("add plain member");
        let member_view = svc
            .evaluate_detail_access(member, plain, UserDetailSource::Conversation { channel_id })
            .await
            .expect("普通成员互查仍可看公开投影");
        assert!(!member_view.can_add_friend, "普通成员之间受群策略限制");
        assert_eq!(member_view.deny_reason, Some("group_policy"));

        let _ = sqlx::query("DELETE FROM privchat_groups WHERE group_id = $1")
            .bind(channel_id as i64)
            .execute(svc.channel_service.pool())
            .await;
        cleanup(&svc, channel_id, &[owner, member, plain]).await;
    }
}
