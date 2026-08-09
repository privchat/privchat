// 附件秒传的真库门禁（单表模型）。
//
// 模型就一句话：**物理文件只存一份，数据库每人一行**。
//   - 每个用户有自己的 file_id / uploader_id / 业务绑定
//   - 多行可以指向同一个 file_path
//   - 消息只引用**自己那条** file_id，与别人的消息无关
//   - 删除只删自己那行；还有别人指着同一个物理文件时，文件留着
//
// 「转发」没有独立实现：它就是当前用户重新发一条同样的消息，附件走这条路径。

use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;

use privchat::model::file_upload::{FileMetadata, FileType};
use privchat::repository::FileUploadRepository;

const OWNER: i64 = 9_980_001;
const OTHER: i64 = 9_980_002;
const SHA: &str = "d01f1b584be7a9e4acbaac536abfa9f00d9d33fb62a5ce76c54a25ee096908bd";
const SHARED_PATH: &str = "/tmp/privchat-dedup-test/photo.png";

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

async fn cleanup(pool: &sqlx::PgPool) {
    sqlx::query("DELETE FROM privchat_file_uploads WHERE uploader_id = ANY($1)")
        .bind(&vec![OWNER, OTHER])
        .execute(pool)
        .await
        .expect("clean uploads");
    for uid in [OWNER, OTHER] {
        sqlx::query(
            "INSERT INTO privchat_users (user_id, username, display_name, qr_key) \
             VALUES ($1, $2, $2, $3) ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(uid)
        .bind(format!("dedup_{uid}"))
        .bind(privchat::rpc::qr::generate_qr_key())
        .execute(pool)
        .await
        .expect("user");
    }
}

/// 原始上传：OWNER 传了一份文件。
async fn seed_original(repo: &FileUploadRepository) -> FileMetadata {
    let file_id = repo.next_file_id().await.expect("file id");
    let meta = FileMetadata {
        file_id,
        original_filename: "photo.png".to_string(),
        file_size: 1024,
        original_size: None,
        file_type: FileType::Image,
        mime_type: "image/png".to_string(),
        file_path: SHARED_PATH.to_string(),
        storage_source_id: 0,
        uploader_id: OWNER as u64,
        uploader_ip: None,
        uploaded_at: 0,
        width: None,
        height: None,
        // 秒传身份：客户端声明的最终明文内容摘要。
        file_hash: Some(SHA.to_string()),
        business_type: Some("message".to_string()),
        // ⚠️ 必须给真值：留 None 的话「新行不继承别人的绑定」那条断言是空的——
        // 复制不复制都得到 None，把 business_id 一起复制过去测试照样绿。
        business_id: Some("7777".to_string()),
        encryption_version: 0,
        cek: None,
    };
    repo.insert(&meta).await.expect("insert original");
    meta
}

/// 同一份内容第二次发送：查得到，复制出**自己的**一行，物理文件不动。
#[tokio::test]
async fn a_second_sender_gets_their_own_row_over_the_same_file() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());

    let original = seed_original(&repo).await;

    // 探测：按 内容摘要 + 类型 + 大小 找。
    let found = repo
        .find_by_content(SHA, FileType::Image.as_str(), 1024)
        .await
        .expect("probe")
        .expect("命中已有内容");
    assert_eq!(found.file_id, original.file_id);

    // 取用：给 OTHER 复制一行。
    let mine = repo
        .copy_for_user(&found, OTHER as u64, "message")
        .await
        .expect("copy");

    assert_ne!(mine, original.file_id, "拿到的必须是自己的新 file_id");

    let rows: Vec<(i64, i64, String, Option<String>)> = sqlx::query_as(
        "SELECT file_id, uploader_id, file_path, business_id FROM privchat_file_uploads \
         WHERE file_id = ANY($1) ORDER BY file_id",
    )
    .bind(&vec![original.file_id as i64, mine as i64])
    .fetch_all(pool.as_ref())
    .await
    .expect("read rows");

    assert_eq!(rows.len(), 2, "两个人各一行");
    for (file_id, uploader_id, path, business_id) in &rows {
        assert_eq!(path, SHARED_PATH, "两行指向同一个物理文件——这就是「不重传」");
        if *file_id == mine as i64 {
            assert_eq!(*uploader_id, OTHER, "新行归属请求者自己");
            assert!(
                business_id.is_none(),
                "新行不继承别人的业务绑定：它要绑到我自己的那条消息上",
            );
        }
    }

    cleanup(&pool).await;
}

/// 还有别人指着同一个物理文件时，只删自己那行。
#[tokio::test]
async fn deleting_one_row_keeps_the_file_while_others_point_at_it() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());

    let original = seed_original(&repo).await;
    let mine = repo
        .copy_for_user(&original, OTHER as u64, "message")
        .await
        .expect("copy");

    assert!(
        repo.other_rows_share_path(mine, SHARED_PATH)
            .await
            .expect("count"),
        "两行都在时，删任意一行都不该动物理文件",
    );

    repo.delete(mine).await.expect("delete own row");

    assert!(
        !repo
            .other_rows_share_path(original.file_id, SHARED_PATH)
            .await
            .expect("count"),
        "只剩最后一行时，才轮到删物理文件",
    );

    cleanup(&pool).await;
}

/// 身份是 内容摘要 + 类型 + 大小：任一项不同都不算同一份内容。
#[tokio::test]
async fn size_and_type_are_part_of_the_identity() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());

    seed_original(&repo).await;

    assert!(
        repo.find_by_content(SHA, FileType::Image.as_str(), 2048)
            .await
            .expect("probe")
            .is_none(),
        "大小不同不能命中",
    );
    assert!(
        repo.find_by_content(SHA, FileType::File.as_str(), 1024)
            .await
            .expect("probe")
            .is_none(),
        "类型不同不能命中",
    );

    cleanup(&pool).await;
}

/// 老数据不会误命中。
///
/// 存量行的 file_hash 是 `hash:<u64>`（DefaultHasher，跨 Rust 版本都不稳定），
/// 与 64 位十六进制摘要不可能相等——所以老文件只是命不中，不会张冠李戴。
#[tokio::test]
async fn legacy_hashes_never_match() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());

    let mut legacy = seed_original(&repo).await;
    legacy.file_hash = Some("hash:12345678901234567890".to_string());
    sqlx::query("UPDATE privchat_file_uploads SET file_hash = $2 WHERE file_id = $1")
        .bind(legacy.file_id as i64)
        .bind(legacy.file_hash.as_deref())
        .execute(pool.as_ref())
        .await
        .expect("legacy hash");

    assert!(
        repo.find_by_content(SHA, FileType::Image.as_str(), 1024)
            .await
            .expect("probe")
            .is_none(),
        "老摘要格式与内容摘要不可能相等",
    );

    cleanup(&pool).await;
}
