// 附件秒传的真库门禁。
//
// 「转发」在本产品里没有独立实现：它就是当前用户重新发一条同样的消息，
// 附件靠这条路径复用。所以这几条用例同时就是转发的门禁。
//
// 覆盖：同内容只存一份物理对象 / 处理版本参与身份 / 命中后拿到的是**自己的**新句柄 /
// 并发登记不会插出两行 / 不能凭摘要拿到别人的文件。

use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;

use privchat::service::media_blob_service::{find_blob, register_blob, BlobIdentity};

const OWNER: i64 = 9_970_001;
const OTHER: i64 = 9_970_002;
const SHA_A: &str = "d01f1b584be7a9e4acbaac536abfa9f00d9d33fb62a5ce76c54a25ee096908bd";
const SHA_B: &str = "0000000000000000000000000000000000000000000000000000000000000001";

fn fixture_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn pool() -> Option<Arc<sqlx::PgPool>> {
    let url = privchat::require_test_database_url()?;
    Some(Arc::new(
        PgPoolOptions::new()
            .max_connections(6)
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
        .expect("clean handles");
    sqlx::query("DELETE FROM privchat_media_blobs WHERE content_sha256 = ANY($1)")
        .bind(&vec![SHA_A.to_string(), SHA_B.to_string()])
        .execute(pool)
        .await
        .expect("clean blobs");
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

async fn register(pool: &sqlx::PgPool, sha: &str, version: i32) -> i64 {
    let identity = BlobIdentity::parse(sha, version).expect("identity");
    register_blob(
        pool,
        &identity,
        "/tmp/blob.bin",
        0,
        1024,
        "image/png",
        0,
        None,
    )
    .await
    .expect("register")
    .blob_id
}

/// 同一份内容只登记一份物理对象。
#[tokio::test]
async fn the_same_content_is_stored_once() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;

    let first = register(&pool, SHA_A, 0).await;
    let second = register(&pool, SHA_A, 0).await;
    assert_eq!(first, second, "同内容第二次登记必须返回同一个对象，不是新插一行");

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM privchat_media_blobs WHERE content_sha256 = $1")
            .bind(SHA_A)
            .fetch_one(pool.as_ref())
            .await
            .expect("count");
    assert_eq!(count, 1, "库里只能有一行");

    cleanup(&pool).await;
}

/// 🔴 处理版本参与身份：换了压缩算法就是另一份字节。
#[tokio::test]
async fn a_different_transform_version_gets_its_own_object() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;

    let v0 = register(&pool, SHA_A, 0).await;
    let v1 = register(&pool, SHA_A, 1).await;
    assert_ne!(
        v0, v1,
        "同摘要不同处理版本必须是两个对象——混在一起会让新版本取回旧编码的画质",
    );

    cleanup(&pool).await;
}

/// 并发登记同一份内容不会插出两行。
///
/// 先查后插在并发下会：两边都查不到 → 都插 → 唯一索引把后一个打成错误，
/// 而那次上传其实是成功的。所以实现必须是 `ON CONFLICT ... RETURNING`。
#[tokio::test]
async fn concurrent_registration_converges_on_one_object() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let (p1, p2) = (pool.clone(), pool.clone());
    let (b1, b2) = (barrier.clone(), barrier.clone());
    let (a, b) = tokio::join!(
        async move {
            b1.wait().await;
            register(&p1, SHA_B, 0).await
        },
        async move {
            b2.wait().await;
            register(&p2, SHA_B, 0).await
        },
    );
    assert_eq!(a, b, "并发登记必须收敛到同一个对象");

    cleanup(&pool).await;
}

/// 命中之后拿到的是**自己的**新句柄，而不是别人的 file_id。
///
/// 🔴 这条是整个秒传的安全支点：把别人的 file_id 发回去，等于把别人的文件记录
/// 交给他，后续发消息时的归属校验（uploader_id = sender_id）也会被绕过。
#[tokio::test]
async fn a_hit_yields_the_requesters_own_handle() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;

    let blob_id = register(&pool, SHA_A, 0).await;
    let blob = find_blob(&pool, &BlobIdentity::parse(SHA_A, 0).unwrap())
        .await
        .expect("find")
        .expect("blob exists");
    assert_eq!(blob.blob_id, blob_id);

    let repo = privchat::repository::FileUploadRepository::new(pool.clone());
    let owner_file = repo
        .create_handle_for_blob(&blob, OWNER as u64, "a.png", "image", "message")
        .await
        .expect("owner handle");
    let other_file = repo
        .create_handle_for_blob(&blob, OTHER as u64, "a.png", "image", "message")
        .await
        .expect("other handle");

    assert_ne!(owner_file, other_file, "两个人拿到的是两个不同的句柄");

    let rows: Vec<(i64, i64, Option<i64>)> = sqlx::query_as(
        "SELECT file_id, uploader_id, blob_id FROM privchat_file_uploads \
         WHERE file_id = ANY($1) ORDER BY file_id",
    )
    .bind(&vec![owner_file as i64, other_file as i64])
    .fetch_all(pool.as_ref())
    .await
    .expect("read handles");
    assert_eq!(rows.len(), 2);
    for (file_id, uploader_id, handle_blob) in rows {
        assert_eq!(
            handle_blob,
            Some(blob_id),
            "两个句柄指向同一个物理对象——这就是「不重新上传」",
        );
        let expected = if file_id == owner_file as i64 { OWNER } else { OTHER };
        assert_eq!(uploader_id, expected, "句柄归属必须是请求者自己");
    }

    // 物理对象仍然只有一份。
    let blobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM privchat_media_blobs WHERE content_sha256 = $1")
            .bind(SHA_A)
            .fetch_one(pool.as_ref())
            .await
            .expect("count");
    assert_eq!(blobs, 1, "两个人发同一张图，字节只存一份");

    cleanup(&pool).await;
}

/// 脏摘要当场拒绝，而不是静默永不命中。
#[tokio::test]
async fn a_malformed_digest_is_refused_up_front() {
    // 旧实现写的是 `hash:<u64>`（DefaultHasher，跨 Rust 版本都不稳定）。
    // 放进来不会报错，只会让秒传永远不命中——表现成「怎么每次都重传」，很难查。
    assert!(BlobIdentity::parse("hash:12345678901234567890", 0).is_err());
    assert!(BlobIdentity::parse("", 0).is_err());
}
