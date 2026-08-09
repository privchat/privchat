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
        .find_by_content(SHA)
        .await
        .expect("probe")
        .expect("命中已有内容");
    assert_eq!(found.file_id, original.file_id);

    // 取用：给 OTHER 复制一行。
    let mine = repo
        .copy_for_user(&found, OTHER as u64, "message", None)
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
        .copy_for_user(&original, OTHER as u64, "message", None)
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

/// 并发首传收敛：两个人同时传同一份内容，最终只能有一份物理文件被指向。
///
/// 🔴 两边预检都没命中 → 各写了一份对象。收尾时必须在**内容锁**里再查一次，
/// 后到的那个改指向先到的那份 path 并删掉自己的。少了这一步，
/// 「物理文件只存一份」在并发下就是一句空话——而并发正是它最该成立的时候。
#[tokio::test]
async fn concurrent_first_uploads_converge_on_one_physical_file() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;

    // 模拟收尾阶段的判定：同一把内容锁下，先到的落自己的 path，后到的看见并改指向。
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut paths = Vec::new();
    let (p1, p2) = (pool.clone(), pool.clone());
    let (b1, b2) = (barrier.clone(), barrier.clone());

    let (a, b) = tokio::join!(
        async move {
            b1.wait().await;
            converge(&p1, OWNER, "/tmp/privchat-dedup-test/a.bin").await
        },
        async move {
            b2.wait().await;
            converge(&p2, OTHER, "/tmp/privchat-dedup-test/b.bin").await
        },
    );
    paths.push(a);
    paths.push(b);

    assert_eq!(
        paths[0], paths[1],
        "两次并发首传必须收敛到同一个物理文件路径",
    );

    cleanup(&pool).await;
}

/// 走**生产那个函数**（`converge_upload`）判定这次上传落到哪个物理文件上。
///
/// 🔴 此前这里抄了一份生产 SQL。抄件只能证明抄件自洽：改了生产的判据，
/// 先红的是测试，而测试证明不了产品。现在两边调的是同一个函数。
async fn converge(pool: &sqlx::PgPool, uploader: i64, my_path: &str) -> String {
    use privchat::service::file_service::{converge_upload, UploadPlacement};

    let repo = FileUploadRepository::new(Arc::new(pool.clone()));
    let file_id = repo.next_file_id().await.expect("file id");
    let mut tx = pool.begin().await.expect("tx");

    let placement = converge_upload(
        &mut tx,
        &UploadPlacement {
            stored_sha256: SHA.to_string(),
            encryption_version: 0,
            my_path: my_path.to_string(),
            my_source_id: 0,
            my_cek: None,
        },
    )
    .await
    .expect("converge");

    // 🔴 把「判完还没写」的窗口撑开。没有这一下，两个事务会被时序自然串开，
    // 于是把内容锁删掉测试照样绿——我第一版就是这样，白测一轮。
    // 有锁时后到者根本进不到这里（它还堵在锁上），所以这段停顿不会造成死等。
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    sqlx::query(
        "INSERT INTO privchat_file_uploads \
         (file_id, original_filename, file_size, file_type, mime_type, file_path, \
          uploader_id, file_hash, business_type) \
         VALUES ($1, 'x.png', 1024, $2, 'image/png', $3, $4, $5, 'message')",
    )
    .bind(file_id as i64)
    .bind(FileType::Image.as_str())
    .bind(&placement.file_path)
    .bind(uploader)
    .bind(SHA)
    .execute(&mut *tx)
    .await
    .expect("insert");
    tx.commit().await.expect("commit");
    placement.file_path
}

/// 秒传取用与删除必须共用同一把 file_path 锁。
///
/// 🔴 不共用会出现：claim 读到源行 → delete 删掉最后一行并删物理文件 →
/// claim 插入新行，结果是一条**指向已被删除文件**的记录。
/// 这里验的是拿到锁之后的复查：源行已经没了就拒绝，而不是插一条死记录。
#[tokio::test]
async fn claiming_a_file_that_was_just_deleted_is_refused() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());

    let original = seed_original(&repo).await;
    // 模拟「claim 已经读到了源行」：拿着这份快照，但库里那行随后被删掉。
    let snapshot = original.clone();
    repo.delete(original.file_id).await.expect("delete source");

    let refused = repo.copy_for_user(&snapshot, OTHER as u64, "message", None).await;
    assert!(
        refused.is_err(),
        "源文件已被删除时必须拒绝，不能留下指向不存在文件的记录",
    );

    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM privchat_file_uploads WHERE file_path = $1",
    )
    .bind(SHARED_PATH)
    .fetch_one(pool.as_ref())
    .await
    .expect("count");
    assert_eq!(rows, 0, "不该凭空多出一行");

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
        repo.find_by_content(SHA)
            .await
            .expect("probe")
            .is_none(),
        "老摘要格式与内容摘要不可能相等",
    );

    cleanup(&pool).await;
}

/// 🔴 claim 响应丢失后重试，必须拿回**同一个** file_id。
///
/// 这是幂等真正的判据。只做到「并发只有一个成功」是不够的：数据库提交了、响应在
/// 网络上丢了，客户端重试如果又插一行，用户就多了一份没人用的记录，
/// 而他手上那条消息引用的还是拿不到的那个 id。
#[tokio::test]
async fn replaying_a_claim_returns_the_same_file_id() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());

    let original = seed_original(&repo).await;
    let key = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";

    let first = repo
        .copy_for_user(&original, OTHER as u64, "message", Some(key))
        .await
        .expect("first claim");
    // 「响应丢了」= 客户端没收到结果，拿同一个 token 又来一次。
    let replay = repo
        .copy_for_user(&original, OTHER as u64, "message", Some(key))
        .await
        .expect("replayed claim");

    assert_eq!(first, replay, "同一个幂等键必须还回同一个 file_id");

    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM privchat_file_uploads \
         WHERE uploader_id = $1 AND claim_key_hash = $2",
    )
    .bind(OTHER)
    .bind(key)
    .fetch_one(pool.as_ref())
    .await
    .expect("count");
    assert_eq!(rows, 1, "重试不能多出一行");

    // 查询接口也要认得它——claim 入口靠它在校验 token 之前就短路返回。
    assert_eq!(
        repo.find_claimed(OTHER as u64, key).await.expect("lookup"),
        Some(first),
    );

    cleanup(&pool).await;
}

/// 老客户端：报**明文**大小，实际上传的密文多 28 字节，必须仍然成功。
///
/// 🔴 我一度把大小核对写成无条件的，那会让新服务端一上线就把所有老客户端的
/// 正常上传全部拒掉。这里调的是**生产那个函数**——上一版在测试里抄了一份同样的
/// 条件，生产改回无条件核对它照样绿。
#[test]
fn a_client_that_declares_no_digest_is_not_size_checked() {
    use privchat::service::file_service::size_check_target;

    // 老客户端：报了明文大小，没有摘要 → 不核对。
    assert_eq!(
        size_check_target(None, Some(1024)),
        None,
        "老客户端报的是明文大小，密文固定多 28 字节；核对它等于禁止所有老客户端上传",
    );
    // 新客户端：两个都按最终 blob 报 → 核对。
    assert_eq!(size_check_target(Some("d0"), Some(1052)), Some(1052));
    // 只有摘要没有大小：没什么可比的。
    assert_eq!(size_check_target(Some("d0"), None), None);
}

/// 🔴 收敛选中了某个路径，而那个路径的最后一行在它拿到路径锁**之前**被删掉：
/// 必须退回用自己刚上传的那份，而不是留下一条指向已删除物理文件的记录。
///
/// 这一臂之前没有覆盖——删掉行再调用的话，按 hash 的查询直接落到 `None` 分支，
/// 走不到复查。这里用两条连接把顺序钉死，不需要给生产函数开测试注入点：
///
///   1. A 先持有目标 `file_path` 的 advisory 锁；
///   2. B 调生产 `converge_upload`：内容锁拿得到、按 hash 查得到旧行，
///      然后**堵在**路径锁上；
///   3. A 删掉最后一行并提交（锁随之释放）；
///   4. B 拿到锁，复查发现路径上已经没有行，退回自己的路径。
#[tokio::test]
async fn convergence_falls_back_when_the_path_is_deleted_while_it_waits() {
    use privchat::service::file_service::{converge_upload, UploadPlacement};

    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());
    let original = seed_original(&repo).await;

    // 1. A 抢先占住路径锁。
    let mut tx_a = pool.begin().await.expect("tx a");
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(SHARED_PATH)
        .execute(&mut *tx_a)
        .await
        .expect("a holds path lock");

    // 2. B 去收敛：会查到旧行，然后堵在路径锁上。
    let pool_b = pool.clone();
    let b = tokio::spawn(async move {
        let mut tx = pool_b.begin().await.expect("tx b");
        let placement = converge_upload(
            &mut tx,
            &UploadPlacement {
                stored_sha256: SHA.to_string(),
                encryption_version: 0,
                my_path: "/tmp/privchat-dedup-test/mine.bin".to_string(),
                my_source_id: 0,
                my_cek: None,
            },
        )
        .await
        .expect("converge");
        tx.rollback().await.ok();
        placement
    });

    // 确认 B 确实在等锁——不等的话下面的删除就没卡在正确的位置上，
    // 这条测试会退化成「先删再收敛」，也就走不到复查。
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!b.is_finished(), "B 应当堵在路径锁上，而不是已经跑完");

    // 3. A 删掉最后一行并提交，释放锁。
    sqlx::query("DELETE FROM privchat_file_uploads WHERE file_id = $1")
        .bind(original.file_id as i64)
        .execute(&mut *tx_a)
        .await
        .expect("delete last row");
    tx_a.commit().await.expect("commit a");

    // 4. B 拿到锁后复查失败，退回自己那份。
    let placement = b.await.expect("join b");
    assert_eq!(
        placement.file_path, "/tmp/privchat-dedup-test/mine.bin",
        "等锁期间路径上的最后一行被删了，必须退回自己刚上传的那份",
    );
    assert!(!placement.duplicate, "自己那份现在是唯一的一份，不能删");

    cleanup(&pool).await;
}

/// 🔴 claim 的**生产入口**：token 已被消费之后重放，必须返回同一个 file_id。
///
/// 之前这条只测到仓储层。仓储层看不见「幂等查询排在 token 校验之前」这个顺序——
/// 有人把它挪到后面，仓储测试照样绿，而实际行为是：成功过的 token 已被消费，
/// 再去校验只会得到「无效」，客户端永远拿不回那条记录。
///
/// 这里调的是 `claim_existing_file` 本身，它只依赖文件服务和 token 服务两样东西，
/// 不需要整个 RpcServiceContext。
#[tokio::test]
async fn replaying_a_claim_through_the_service_returns_the_same_file_id() {
    use privchat::service::file_claim_service::claim_existing_file;
    use privchat::service::upload_token_service::{
        UploadIdentity, UploadTokenPurpose, UploadTokenService,
    };
    use privchat::service::FileService;

    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;

    let repo = FileUploadRepository::new(pool.clone());
    let original = seed_original(&repo).await;

    let file_service = Arc::new(FileService::new(Vec::new(), 0, pool.clone()));
    let token_service = Arc::new(UploadTokenService::new());
    let token = token_service
        .generate_token(
            OTHER as u64,
            FileType::Image,
            10 * 1024 * 1024,
            "message".to_string(),
            Some("photo.png".to_string()),
            UploadIdentity {
                sha256: Some(SHA.to_string()),
                declared_size: Some(1024),
                mime_type: Some("image/png".to_string()),
                transform_version: 0,
            },
            // 预检命中签发的就是这种用途。
            UploadTokenPurpose::ClaimExisting,
        )
        .await
        .expect("token");

    let first = claim_existing_file(&file_service, &token_service, OTHER as u64, &token.token, SHA)
        .await
        .expect("first claim");
    assert_ne!(first.file_id, original.file_id, "拿到的是自己的新 file_id");

    // token 此刻已被消费——这正是重放路径的前提。
    assert!(
        token_service.validate_token(&token.token).await.is_err(),
        "第一次成功之后 token 应当已经作废",
    );

    // 「响应丢了」：客户端拿同一个 token 再来一次。
    let replay = claim_existing_file(&file_service, &token_service, OTHER as u64, &token.token, SHA)
        .await
        .expect("replayed claim must succeed even though the token is spent");
    assert_eq!(first.file_id, replay.file_id, "重放必须返回同一个 file_id");

    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM privchat_file_uploads WHERE uploader_id = $1",
    )
    .bind(OTHER)
    .fetch_one(pool.as_ref())
    .await
    .expect("count");
    assert_eq!(rows, 1, "重放不能多出一行");

    cleanup(&pool).await;
}
