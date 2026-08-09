// 单条转发在**提交层**的门禁。
//
// 🔴 这是此前完全没有覆盖的那一段：转发一张**别人上传的**图片。
//
// 提交事务里有一道归属守卫，要求 `uploader_id = sender_id`——它防的是「客户端报一个
// 别人的 file_id 把附件劫走」。转发恰恰要复用别人的文件，所以走的是另一条分支：
// `AttachmentOrigin::CopiedFromExistingMessage` 只加引用、不改归属。
//
// 之前所有测试都停在这道守卫**之前**，于是「图片转发」在提交这一步会被拒，而测试全绿。
// 这里两条一起验：转发分支必须过，新上传分支对同一份数据必须被拒。

use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;

use privchat::model::message::Message;
use privchat::repository::message_repo::{
    AtomicMessageCommitRequest, AttachmentOrigin, ClientRegistryClaim,
};
use privchat::repository::PgMessageRepository;
use privchat_protocol::{MediaRef, MediaRole};

const OWNER: i64 = 9_960_001; // 上传者 = 源消息发送者
const FORWARDER: i64 = 9_960_002; // 转发人，不是文件的上传者
const SOURCE_CHANNEL: i64 = 9_961_001;
const TARGET_CHANNEL: i64 = 9_961_002;
const SOURCE_MESSAGE: i64 = 9_962_001;
const PEER: i64 = 9_960_003; // 目标会话的另一方
const FILE_ID: i64 = 9_963_001;

fn fixture_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn pool() -> Option<Arc<sqlx::PgPool>> {
    let url = privchat::require_test_database_url()?;
    Some(Arc::new(
        PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap_or_else(|e| panic!("连接测试数据库失败（{url}）: {e}")),
    ))
}

async fn seed(pool: &sqlx::PgPool) {
    cleanup(pool).await;
    for (uid, name) in [(OWNER, "fwd_owner"), (FORWARDER, "fwd_forwarder"), (PEER, "fwd_peer")] {
        sqlx::query(
            "INSERT INTO privchat_users (user_id, username, display_name, qr_key) \
             VALUES ($1, $2, $2, $3) ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(uid)
        .bind(name)
        .bind(privchat::rpc::qr::generate_qr_key())
        .execute(pool)
        .await
        .expect("user");
    }
    // 🔴 DB 的 channel_type 与 wire 差 1：DB Direct=0，wire Direct=1。
    // 建表约束还要求 Direct 必须带上两个 direct_user。
    // ⚠️ 两个会话必须用**不同**的 direct 用户对：同一对会撞 direct 唯一索引，
    // 而 `ON CONFLICT DO NOTHING` 会把第二条静默跳过，随后建消息时才炸外键。
    for (cid, a, b) in [
        (SOURCE_CHANNEL, OWNER, FORWARDER),
        (TARGET_CHANNEL, FORWARDER, PEER),
    ] {
        sqlx::query(
            "INSERT INTO privchat_channels \
             (channel_id, channel_type, direct_user1_id, direct_user2_id) \
             VALUES ($1, 0, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(cid)
        .bind(a)
        .bind(b)
        .execute(pool)
        .await
        .expect("channel");
    }

    // OWNER 上传的文件，已绑定到源消息。
    sqlx::query(
        "INSERT INTO privchat_file_uploads \
         (file_id, original_filename, file_size, file_type, mime_type, file_path, \
          uploader_id, business_type, business_id) \
         VALUES ($1, 'photo.png', 1024, 'image', 'image/png', '/tmp/photo.png', $2, \
                 'message', $3)",
    )
    .bind(FILE_ID)
    .bind(OWNER)
    .bind(SOURCE_MESSAGE.to_string())
    .execute(pool)
    .await
    .expect("file");

    sqlx::query(
        "INSERT INTO privchat_messages \
         (message_id, channel_id, sender_id, pts, content, message_type, metadata, \
          created_at, updated_at) \
         VALUES ($1, $2, $3, 1, 'photo', 2, '{}'::jsonb, now_millis(), now_millis())",
    )
    .bind(SOURCE_MESSAGE)
    .bind(SOURCE_CHANNEL)
    .bind(OWNER)
    .execute(pool)
    .await
    .expect("source message");
}

async fn cleanup(pool: &sqlx::PgPool) {
    // 🔴 幂等注册表也要清：`client_registry_claim` 用 (uid, device, local_message_id)
    // 去重，上一次跑留下的 claim 会让下一次提交直接报
    // 「client registry conflict without message projection」——那看起来像功能坏了，
    // 其实是脏 fixture。
    sqlx::query("DELETE FROM privchat_client_msg_registry WHERE sender_id = $1")
        .bind(FORWARDER)
        .execute(pool)
        .await
        .expect("clean registry");
    sqlx::query("DELETE FROM privchat_message_file_refs WHERE file_id = $1")
        .bind(FILE_ID)
        .execute(pool)
        .await
        .expect("clean refs");
    sqlx::query("DELETE FROM privchat_messages WHERE channel_id = ANY($1)")
        .bind(&vec![SOURCE_CHANNEL, TARGET_CHANNEL])
        .execute(pool)
        .await
        .expect("clean messages");
    sqlx::query("DELETE FROM privchat_file_uploads WHERE file_id = $1")
        .bind(FILE_ID)
        .execute(pool)
        .await
        .expect("clean file");
    sqlx::query("DELETE FROM privchat_channels WHERE channel_id = ANY($1)")
        .bind(&vec![SOURCE_CHANNEL, TARGET_CHANNEL])
        .execute(pool)
        .await
        .expect("clean channels");
}

fn copy_request(message_id: i64, origin: AttachmentOrigin) -> AtomicMessageCommitRequest {
    let now = chrono::Utc::now();
    let message = Message {
        message_id: message_id as u64,
        channel_id: TARGET_CHANNEL as u64,
        sender_id: FORWARDER as u64,
        pts: None,
        local_message_id: Some(message_id as u64),
        content: "photo".to_string(),
        message_type: privchat_protocol::ContentMessageType::Image,
        metadata: serde_json::json!({}),
        reply_to_message_id: None,
        created_at: now,
        updated_at: now,
        deleted: false,
        deleted_at: None,
        revoked: false,
        revoked_at: None,
        revoked_by: None,
    };
    let event = privchat_protocol::CanonicalTimelineEvent::from_legacy(
        "image",
        &serde_json::json!({ "content": "photo" }),
        message_id as u64,
        FORWARDER as u64,
        now.timestamp_millis(),
    )
    .expect("canonical event")
    .expect("canonical event present");

    AtomicMessageCommitRequest {
        message,
        dedup_key: None,
        client_registry_claim: Some(ClientRegistryClaim {
            device_id: "fwd-device".to_string(),
            decision: "accepted".to_string(),
        }),
        attachment_refs: vec![MediaRef {
            file_id: FILE_ID as u64,
            role: MediaRole::Original,
            ordinal: 0,
        }],
        attachment_origin: origin,
        forward_precondition: None,
        channel_type: 1,
        event,
        sender_username: None,
    }
}

/// 转发别人上传的图片：提交必须成功，并且新消息挂上同一个 file_id。
#[tokio::test]
async fn forwarding_someone_elses_photo_commits_and_shares_the_file() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    seed(&pool).await;

    let repo = PgMessageRepository::new(pool.clone());
    let copy_id = 9_962_101;
    repo.create_message_and_commit_atomic(copy_request(copy_id, AttachmentOrigin::CopiedFromExistingMessage))
        .await
        .expect("转发副本必须能提交——归属守卫不该拦住复用");

    let refs: Vec<(i64,)> = sqlx::query_as(
        "SELECT message_id FROM privchat_message_file_refs WHERE file_id = $1 ORDER BY message_id",
    )
    .bind(FILE_ID)
    .fetch_all(pool.as_ref())
    .await
    .expect("read refs");
    assert!(
        refs.iter().any(|(id,)| *id == copy_id),
        "副本必须引用同一个 file_id——这正是「不重新上传」的含义",
    );

    // 归属不变：文件仍属于上传者，转发不夺走所有权。
    let uploader: i64 =
        sqlx::query_scalar("SELECT uploader_id FROM privchat_file_uploads WHERE file_id = $1")
            .bind(FILE_ID)
            .fetch_one(pool.as_ref())
            .await
            .expect("read uploader");
    assert_eq!(uploader, OWNER, "转发不改变文件归属");

    cleanup(&pool).await;
}

/// 同一份数据按「新上传」提交必须被拒。
///
/// 这条是上一条的对照：证明放行来自转发分支本身，而不是守卫整体失效了。
#[tokio::test]
async fn claiming_someone_elses_file_as_a_fresh_upload_is_refused() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    seed(&pool).await;

    let repo = PgMessageRepository::new(pool.clone());
    let result = repo
        .create_message_and_commit_atomic(copy_request(9_962_102, AttachmentOrigin::FreshUpload))
        .await;
    assert!(
        result.is_err(),
        "报别人的 file_id 当作自己新上传的，必须被归属守卫拒绝",
    );

    cleanup(&pool).await;
}
