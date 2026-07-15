// 附件加密 v1 — file/get_url 访问授权 RC-gate（DB-backed）。
//
// 复刻 `src/rpc/file/get_url.rs` 的鉴权组合（get_by_file_id → bound_message_id →
// message_repo.get_channel_id → channel_service.is_channel_member → authorize_file_access），
// 用真实 repo/service 打到真库，覆盖 ATTACHMENT_ENCRYPTION_SPEC §授权 的 7 条规则：
//   1. 发送附件后 file.business_id = message_id（绑定机制 / update_business）
//   2. thumbnail_file_id 同样绑定 message_id
//   3. channel member 调 get_url 成功，拿到 cek
//   4. 非 channel member forbidden
//   5. pending file（未绑定 business）只有 uploader 可 get_url
//   6. broken business_id（指向不存在的 message）→ forbidden（不 fallback 放行）
//   7. legacy encryption_version=0 授权规则一样，但 cek=null
//
// gate：未配 PRIVCHAT_TEST_DATABASE_URL / DATABASE_URL 时跳过。

use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;

use privchat::model::file_upload::{FileMetadata, FileType};
use privchat::repository::{FileUploadRepository, PgChannelRepository, PgMessageRepository};
use privchat::service::file_service::authorize_file_access;
use privchat::service::ChannelService;

// 测试 ID 命名空间（避免与其它测试 / 真实数据冲突）。
const MEMBER_UID: i64 = 9_940_001;
const UPLOADER_UID: i64 = 9_940_002;
const STRANGER_UID: i64 = 9_940_003;
const CHANNEL_ID: i64 = 9_941_001;
const MESSAGE_ID: i64 = 9_942_001;
const MISSING_MESSAGE_ID: i64 = 9_942_999; // 故意不建对应 message 行
const FILE_V1_BOUND: u64 = 9_943_001;
const FILE_V0_BOUND: u64 = 9_943_002;
const FILE_THUMB_BOUND: u64 = 9_943_003;
const FILE_PENDING: u64 = 9_943_004;
const FILE_BROKEN: u64 = 9_943_005;
const FILE_BIND_MAIN: u64 = 9_943_006;
const FILE_BIND_THUMB: u64 = 9_943_007;

async fn open_test_pool() -> Option<Arc<sqlx::PgPool>> {
    let url = std::env::var("PRIVCHAT_TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .ok()?;
    Some(Arc::new(pool))
}

async fn ensure_user(pool: &sqlx::PgPool, user_id: i64, username: &str) {
    let qr_key = privchat::rpc::qr::generate_qr_key();
    sqlx::query(
        r#"
        INSERT INTO privchat_users (user_id, username, display_name, qr_key)
        VALUES ($1, $2, $2, $3)
        ON CONFLICT (user_id) DO UPDATE SET username = EXCLUDED.username
        "#,
    )
    .bind(user_id)
    .bind(username)
    .bind(&qr_key)
    .execute(pool)
    .await
    .expect("ensure user");
}

/// Direct（私聊）频道：channel_type=0，direct_user1/2 必填，group_id NULL（满足 CHECK 约束）。
async fn ensure_direct_channel(pool: &sqlx::PgPool, channel_id: i64, u1: i64, u2: i64) {
    sqlx::query(
        r#"
        INSERT INTO privchat_channels (channel_id, channel_type, direct_user1_id, direct_user2_id)
        VALUES ($1, 0, $2, $3)
        ON CONFLICT (channel_id) DO NOTHING
        "#,
    )
    .bind(channel_id)
    .bind(u1)
    .bind(u2)
    .execute(pool)
    .await
    .expect("ensure direct channel");
}

async fn ensure_participant(pool: &sqlx::PgPool, channel_id: i64, user_id: i64) {
    sqlx::query(
        r#"
        INSERT INTO privchat_channel_participants (channel_id, user_id, role, joined_at, left_at)
        VALUES ($1, $2, 2, now_millis(), NULL)
        ON CONFLICT (channel_id, user_id) DO UPDATE SET left_at = NULL
        "#,
    )
    .bind(channel_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("ensure participant");
}

async fn ensure_message(pool: &sqlx::PgPool, message_id: i64, channel_id: i64, sender_id: i64) {
    sqlx::query(
        r#"
        INSERT INTO privchat_messages (message_id, channel_id, sender_id, pts, message_type, content)
        VALUES ($1, $2, $3, 1, 1, '[image]')
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(message_id)
    .bind(channel_id)
    .bind(sender_id)
    .execute(pool)
    .await
    .expect("ensure message");
}

fn file_meta(
    file_id: u64,
    uploader_id: i64,
    encryption_version: i32,
    cek: Option<&str>,
    business_id: Option<i64>,
) -> FileMetadata {
    FileMetadata {
        file_id,
        original_filename: "a.png".to_string(),
        file_size: 64,
        original_size: None,
        file_type: FileType::Image,
        mime_type: "image/png".to_string(),
        file_path: format!("public/chat/message/202601/test_{file_id}"),
        storage_source_id: 0,
        uploader_id: uploader_id as u64,
        uploader_ip: None,
        uploaded_at: 1_767_200_000_000,
        width: None,
        height: None,
        file_hash: None,
        business_type: business_id.map(|_| "message".to_string()),
        business_id: business_id.map(|id| id.to_string()),
        encryption_version,
        cek: cek.map(|s| s.to_string()),
    }
}

/// 复刻 get_url.rs 的鉴权决策；返回 (authorized, cek_if_returned, encryption_version)。
async fn resolve_get_url(
    file_repo: &FileUploadRepository,
    msg_repo: &PgMessageRepository,
    channel_service: &ChannelService,
    file_id: u64,
    user_id: u64,
) -> (bool, Option<String>, i32) {
    let meta = file_repo
        .get_by_file_id(file_id)
        .await
        .expect("query file")
        .expect("file exists");

    let bound_message_id = meta
        .business_id
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|id| *id > 0);

    let member_of_message_channel = match bound_message_id {
        Some(message_id) => match msg_repo.get_channel_id(message_id).await {
            Ok(Some(channel_id)) => Some(
                channel_service
                    .is_channel_member(channel_id, user_id)
                    .await
                    .unwrap_or(false),
            ),
            _ => None,
        },
        None => None,
    };

    let authorized = authorize_file_access(
        user_id,
        meta.uploader_id,
        bound_message_id,
        member_of_message_channel,
    );
    let cek = if authorized { meta.cek.clone() } else { None };
    (authorized, cek, meta.encryption_version)
}

async fn cleanup(pool: &sqlx::PgPool, file_repo: &FileUploadRepository) {
    for fid in [
        FILE_V1_BOUND,
        FILE_V0_BOUND,
        FILE_THUMB_BOUND,
        FILE_PENDING,
        FILE_BROKEN,
        FILE_BIND_MAIN,
        FILE_BIND_THUMB,
    ] {
        let _ = file_repo.delete(fid).await;
    }
    // message / participant 随 channel CASCADE；先删 channel 再删 user。
    let _ = sqlx::query("DELETE FROM privchat_channels WHERE channel_id = $1")
        .bind(CHANNEL_ID)
        .execute(pool)
        .await;
    for uid in [MEMBER_UID, UPLOADER_UID, STRANGER_UID] {
        let _ = sqlx::query("DELETE FROM privchat_users WHERE user_id = $1")
            .bind(uid)
            .execute(pool)
            .await;
    }
}

async fn seed_common(pool: &sqlx::PgPool) {
    ensure_user(pool, MEMBER_UID, "attach_authz_member").await;
    ensure_user(pool, UPLOADER_UID, "attach_authz_uploader").await;
    ensure_user(pool, STRANGER_UID, "attach_authz_stranger").await;
    ensure_direct_channel(pool, CHANNEL_ID, MEMBER_UID, UPLOADER_UID).await;
    ensure_participant(pool, CHANNEL_ID, MEMBER_UID).await;
    ensure_participant(pool, CHANNEL_ID, UPLOADER_UID).await;
    ensure_message(pool, MESSAGE_ID, CHANNEL_ID, UPLOADER_UID).await;
}

#[tokio::test]
async fn attachment_get_url_authz_rc_gate() {
    let Some(pool) = open_test_pool().await else {
        eprintln!("skip attachment_get_url_authz_rc_gate: DATABASE_URL not configured");
        return;
    };
    let file_repo = FileUploadRepository::new(pool.clone());
    let msg_repo = PgMessageRepository::new(pool.clone());
    let channel_repo = Arc::new(PgChannelRepository::new(pool.clone()));
    let channel_service = ChannelService::new_with_repository(channel_repo);

    cleanup(&pool, &file_repo).await;
    seed_common(&pool).await;

    // 绑定到 channel 内 message 的文件：v1（带 cek）、v0（legacy）、缩略图（v1）。
    file_repo
        .insert(&file_meta(
            FILE_V1_BOUND,
            UPLOADER_UID,
            1,
            Some("cek-v1-main"),
            Some(MESSAGE_ID),
        ))
        .await
        .expect("insert v1 bound");
    file_repo
        .insert(&file_meta(
            FILE_V0_BOUND,
            UPLOADER_UID,
            0,
            None,
            Some(MESSAGE_ID),
        ))
        .await
        .expect("insert v0 bound");
    file_repo
        .insert(&file_meta(
            FILE_THUMB_BOUND,
            UPLOADER_UID,
            1,
            Some("cek-v1-thumb"),
            Some(MESSAGE_ID),
        ))
        .await
        .expect("insert thumb bound");
    // pending：无 business 绑定。
    file_repo
        .insert(&file_meta(
            FILE_PENDING,
            UPLOADER_UID,
            1,
            Some("cek-pending"),
            None,
        ))
        .await
        .expect("insert pending");
    // broken：business_id 指向不存在的 message。
    file_repo
        .insert(&file_meta(
            FILE_BROKEN,
            UPLOADER_UID,
            1,
            Some("cek-broken"),
            Some(MISSING_MESSAGE_ID),
        ))
        .await
        .expect("insert broken");

    // 用 closure 简化断言。
    macro_rules! resolve {
        ($fid:expr, $uid:expr) => {
            resolve_get_url(&file_repo, &msg_repo, &channel_service, $fid, $uid as u64).await
        };
    }

    // 3) channel member 调 get_url 成功，返回 cek。
    let (ok, cek, ver) = resolve!(FILE_V1_BOUND, MEMBER_UID);
    assert!(ok, "case3: member must be authorized");
    assert_eq!(
        cek.as_deref(),
        Some("cek-v1-main"),
        "case3: member gets cek"
    );
    assert_eq!(ver, 1);

    // 4) 非 channel member forbidden（拿不到 cek）。
    let (ok, cek, _) = resolve!(FILE_V1_BOUND, STRANGER_UID);
    assert!(!ok, "case4: stranger must be forbidden");
    assert!(cek.is_none(), "case4: forbidden returns no cek");

    // 7) legacy v0 授权规则一样（member ok / stranger forbidden），但 cek=null。
    let (ok, cek, ver) = resolve!(FILE_V0_BOUND, MEMBER_UID);
    assert!(ok, "case7: v0 member authorized");
    assert!(cek.is_none(), "case7: v0 has no cek");
    assert_eq!(ver, 0);
    let (ok, _, _) = resolve!(FILE_V0_BOUND, STRANGER_UID);
    assert!(!ok, "case7: v0 stranger forbidden (same authz)");

    // 2) 缩略图（独立 file_id）绑定同一 message，授权同主文件。
    let (ok, cek, _) = resolve!(FILE_THUMB_BOUND, MEMBER_UID);
    assert!(ok, "case2: thumbnail file member authorized");
    assert_eq!(cek.as_deref(), Some("cek-v1-thumb"));
    let (ok, _, _) = resolve!(FILE_THUMB_BOUND, STRANGER_UID);
    assert!(!ok, "case2: thumbnail stranger forbidden");

    // 5) pending file（未绑定）只有 uploader 可 get_url。
    let (ok, cek, _) = resolve!(FILE_PENDING, UPLOADER_UID);
    assert!(ok, "case5: pending uploader authorized");
    assert_eq!(cek.as_deref(), Some("cek-pending"));
    let (ok, _, _) = resolve!(FILE_PENDING, MEMBER_UID);
    assert!(!ok, "case5: pending non-uploader forbidden");
    let (ok, _, _) = resolve!(FILE_PENDING, STRANGER_UID);
    assert!(!ok, "case5: pending stranger forbidden");

    // 6) broken binding（business_id 指向不存在 message）→ forbidden，连 uploader 也不放行。
    let (ok, _, _) = resolve!(FILE_BROKEN, UPLOADER_UID);
    assert!(!ok, "case6: broken binding forbidden even for uploader");
    let (ok, _, _) = resolve!(FILE_BROKEN, MEMBER_UID);
    assert!(!ok, "case6: broken binding forbidden for member");

    cleanup(&pool, &file_repo).await;
}

#[tokio::test]
async fn attachment_file_and_thumbnail_bind_to_message() {
    let Some(pool) = open_test_pool().await else {
        eprintln!(
            "skip attachment_file_and_thumbnail_bind_to_message: DATABASE_URL not configured"
        );
        return;
    };
    let file_repo = FileUploadRepository::new(pool.clone());

    cleanup(&pool, &file_repo).await;
    seed_common(&pool).await;

    // 模拟发送：先是 pending（无 business），随后 send_message_handler 绑定到 message。
    file_repo
        .insert(&file_meta(
            FILE_BIND_MAIN,
            UPLOADER_UID,
            1,
            Some("cek-main"),
            None,
        ))
        .await
        .expect("insert main pending");
    file_repo
        .insert(&file_meta(
            FILE_BIND_THUMB,
            UPLOADER_UID,
            1,
            Some("cek-thumb"),
            None,
        ))
        .await
        .expect("insert thumb pending");

    // 1) + 2) update_business 绑定主文件 + 缩略图到 message_id。
    let bound_main = file_repo
        .update_business(FILE_BIND_MAIN, "message", &MESSAGE_ID.to_string())
        .await
        .expect("bind main");
    let bound_thumb = file_repo
        .update_business(FILE_BIND_THUMB, "message", &MESSAGE_ID.to_string())
        .await
        .expect("bind thumb");
    assert!(bound_main && bound_thumb, "both binds affected a row");

    for fid in [FILE_BIND_MAIN, FILE_BIND_THUMB] {
        let meta = file_repo
            .get_by_file_id(fid)
            .await
            .expect("query")
            .expect("exists");
        assert_eq!(
            meta.business_type.as_deref(),
            Some("message"),
            "file {fid} business_type"
        );
        assert_eq!(
            meta.business_id.as_deref(),
            Some(MESSAGE_ID.to_string().as_str()),
            "file {fid} business_id == message_id"
        );
    }

    cleanup(&pool, &file_repo).await;
}
