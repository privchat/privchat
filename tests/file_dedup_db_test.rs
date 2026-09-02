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

use privchat::model::file_upload::{AttachmentObject, FileMetadata, FileType};
use privchat::repository::{FileUploadRepository, ReferenceMetadata};

// 🔴 这里删掉了一组只为旧判据存在的 helper（`InsertBarrier` / `wait_until_blocked_by`
// / `wait_for_claim_at_barrier` / `authorization_deps`）。它们是用来证明"并发撤回被
// claim 持有的消息行锁挡住"的——而 claim 已经不再读消息域，那套跨域加锁连同它的
// 观测脚手架一起没了意义。留着只会让人以为那条链路还在。

const OWNER: i64 = 9_980_001;
const OTHER: i64 = 9_980_002;
/// 私聊/群聊授权用例专用：私聊频道对 (user1, user2) 有唯一索引，
/// 复用 OWNER/OTHER 那一对建不出第二条 DM。
const DIRECT_PEER: i64 = 9_980_011;
const DIRECT_HOST: i64 = 9_980_012;
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
    // 🔴 这里漏掉哪个 uploader，它的残留行就会以 `file_path` 相同的方式，破坏
    // 别的用例「源文件已被删」这类前提——失败点离病因很远，非常难查。
    sqlx::query("DELETE FROM privchat_file_uploads WHERE uploader_id = ANY($1)")
        .bind(&vec![OWNER, OTHER, DIRECT_PEER, DIRECT_HOST, 9_980_031])
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
///
/// 🔴 身份与物理坐标都落在**对象行**上，文件行只是一条引用。
async fn seed_original(repo: &FileUploadRepository) -> FileMetadata {
    let object_id = seed_object(repo.pool(), SHA, SHARED_PATH).await;
    let file_id = repo.next_file_id().await.expect("file id");
    let meta = FileMetadata {
        file_id,
        original_filename: "photo.png".to_string(),
        original_size: None,
        file_type: FileType::Image,
        mime_type: "image/png".to_string(),
        uploader_id: OWNER as u64,
        uploader_ip: None,
        uploaded_at: 0,
        width: None,
        height: None,
        business_type: Some("message".to_string()),
        // ⚠️ 必须给真值：留 None 的话「新引用不继承别人的绑定」那条断言是空的——
        // 复制不复制都得到 None，把 business_id 一起复制过去测试照样绿。
        business_id: Some("7777".to_string()),
        object: object_of(object_id, SHA, SHARED_PATH),
    };
    repo.insert(&meta).await.expect("insert original");
    meta
}

/// 一份已发布对象的行；返回 object_id。
async fn seed_object(pool: &sqlx::PgPool, plaintext_sha256: &str, path: &str) -> u64 {
    // 同一份 fixture 反复跑：孤儿对象会撞 plaintext_sha256 唯一约束。
    let _ = sqlx::query(
        "DELETE FROM privchat_attachment_objects o WHERE o.plaintext_sha256 = $1 \
         AND NOT EXISTS (SELECT 1 FROM privchat_file_uploads u WHERE u.object_id = o.object_id)",
    )
    .bind(plaintext_sha256)
    .execute(pool)
    .await;
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO privchat_attachment_objects \
         (plaintext_sha256, plaintext_size, sealed_sha256, sealed_size, file_path, \
          storage_source_id, format_version, encryption_key_id) \
         VALUES ($1, 1024, $2, 1092, $3, 0, 1, 1) RETURNING object_id",
    )
    .bind(plaintext_sha256)
    .bind("5e".repeat(32))
    .bind(path)
    .fetch_one(pool)
    .await
    .expect("seed object") as u64
}

fn object_of(object_id: u64, plaintext_sha256: &str, path: &str) -> AttachmentObject {
    AttachmentObject {
        object_id,
        plaintext_sha256: plaintext_sha256.to_string(),
        plaintext_size: 1024,
        sealed_sha256: "5e".repeat(32),
        sealed_size: 1092,
        file_path: path.to_string(),
        storage_source_id: 0,
        format_version: 1,
        encryption_key_id: 1,
    }
}

/// 秒传取用：给 `uid` 复制出**自己的**一条引用（口径同 `file_claim_service`）。
///
/// 🔴 元数据来自**取用者自己**（生产里来自他那张 token），不是从源记录抄的：
/// 同一份内容，两个人可以起不同的文件名、报不同的 mime、绑到各自的业务上。
/// 从源记录复制等于把第一个上传者的文件名和业务绑定塞给第二个人。
async fn claim_for(
    repo: &FileUploadRepository,
    object_id: u64,
    uid: u64,
    filename: &str,
    mime: &str,
) -> u64 {
    repo.create_reference(
        object_id,
        uid,
        &ReferenceMetadata {
            original_filename: filename,
            file_type: &FileType::Image,
            mime_type: mime,
            business_type: "message",
        },
        None,
    )
    .await
    .expect("claim")
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
        .find_object_by_plaintext_sha256(SHA)
        .await
        .expect("probe")
        .expect("命中已有内容");
    assert_eq!(found.object_id, original.object.object_id, "命中的必须是同一个物理对象");

    // 取用：给 OTHER 复制一行。
    let mine = claim_for(&repo, found.object_id, OTHER as u64, "mine.png", "image/png").await;

    assert_ne!(mine, original.file_id, "拿到的必须是自己的新 file_id");

    // 物理坐标在对象行上；引用行只带自己的归属与业务字段。
    let rows: Vec<(i64, i64, String, String, Option<String>)> = sqlx::query_as(
        "SELECT u.file_id, u.uploader_id, o.file_path, u.original_filename, u.business_id \
         FROM privchat_file_uploads u \
         JOIN privchat_attachment_objects o ON o.object_id = u.object_id \
         WHERE u.file_id = ANY($1) ORDER BY u.file_id",
    )
    .bind(&vec![original.file_id as i64, mine as i64])
    .fetch_all(pool.as_ref())
    .await
    .expect("read rows");

    assert_eq!(rows.len(), 2, "两个人各一行");
    for (file_id, uploader_id, path, filename, business_id) in &rows {
        assert_eq!(path, SHARED_PATH, "两行指向同一个物理对象——这就是「不重传」");
        if *file_id == mine as i64 {
            assert_eq!(*uploader_id, OTHER, "新行归属请求者自己");
            // 🔴 元数据来自**取用者自己的 token**，不是从源行抄的。
            assert_eq!(filename, "mine.png", "文件名是我自己报的，不是第一个上传者的");
            assert!(
                business_id.is_none(),
                "新行不继承别人的业务绑定：它要绑到我自己的那条消息上",
            );
        } else {
            assert_eq!(filename, "photo.png", "源行不受影响");
        }
    }

    // 物理对象只有一个。
    let objects: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM privchat_attachment_objects WHERE plaintext_sha256 = $1",
    )
    .bind(SHA)
    .fetch_one(pool.as_ref())
    .await
    .expect("count objects");
    assert_eq!(objects, 1, "两条引用，一个物理对象");

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
    let mine = claim_for(&repo, original.object.object_id, OTHER as u64, "mine.png", "image/png").await;

    assert!(
        repo.other_rows_share_object(mine, original.object.object_id)
            .await
            .expect("count"),
        "两行都在时，删任意一行都不该动物理文件",
    );

    repo.delete(mine).await.expect("delete own row");

    assert!(
        !repo
            .other_rows_share_object(original.file_id, original.object.object_id)
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
            // 🔴 判重键是**明文**摘要：密文每次封装都不同，按它判等于秒传只对
            // "自己重发自己"生效。
            plaintext_sha256: SHA.to_string(),
            plaintext_size: 1024,
            sealed_sha256: "5e".repeat(32),
            sealed_size: 1092,
            my_path: my_path.to_string(),
            my_source_id: 0,
            format_version: 1,
            encryption_key_id: 1,
        },
    )
    .await
    .expect("converge");

    // 🔴 把「判完还没写」的窗口撑开。没有这一下，两个事务会被时序自然串开，
    // 于是把内容锁删掉测试照样绿——我第一版就是这样，白测一轮。
    // 有锁时后到者根本进不到这里（它还堵在锁上），所以这段停顿不会造成死等。
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // 引用行只挂 object_id：物理事实在收敛出来的那个对象上。
    sqlx::query(
        "INSERT INTO privchat_file_uploads \
         (file_id, original_filename, file_type, mime_type, object_id, \
          uploader_id, business_type) \
         VALUES ($1, 'x.png', $2, 'image/png', $3, $4, 'message')",
    )
    .bind(file_id as i64)
    .bind(FileType::Image.as_str())
    .bind(placement.object_id as i64)
    .bind(uploader)
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
    // 模拟「claim 已经读到了对象」：拿着这份快照，但那个**物理对象**随后被 GC 掉。
    //
    // 🔴 判据换成删对象，不是删引用行：引用行的删除不再牵动物理对象（那正是
    // 多条引用共享一个对象的意义）。删掉一条引用之后 claim 当然还该成立——
    // 真正必须拒绝的是"对象已经不在了"。
    let snapshot = original.clone();
    repo.delete(original.file_id).await.expect("delete source reference");
    sqlx::query("DELETE FROM privchat_attachment_objects WHERE object_id = $1")
        .bind(snapshot.object.object_id as i64)
        .execute(pool.as_ref())
        .await
        .expect("GC 掉物理对象");

    let refused = repo.create_reference(snapshot.object.object_id, OTHER as u64, &ReferenceMetadata {
            original_filename: &snapshot.original_filename,
            file_type: &snapshot.file_type,
            mime_type: &snapshot.mime_type,
            business_type: "message",
        }, None).await;
    assert!(
        refused.is_err(),
        "物理对象已被删除时必须拒绝，不能留下指向不存在对象的引用",
    );

    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM privchat_file_uploads u \
         JOIN privchat_attachment_objects o ON o.object_id = u.object_id \
         WHERE o.file_path = $1",
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
// 🔴 删掉了 `legacy_hashes_never_match`。
//
// 它盯的是 `privchat_file_uploads.file_hash` 这一列——存量行里放着
// `hash:<u64>`（DefaultHasher）格式的老摘要，用例证明它不会与 64 位十六进制
// 摘要张冠李戴。这一列已经随 migration 032 删除，身份统一搬到
// `privchat_attachment_objects` 上，且判重键换成了明文摘要并带
// `^[0-9a-f]{64}$` 的 CHECK 约束——老格式在数据库层就进不来了。
// 用例失去了它要守的东西，留着只会是一段永远为真的空断言。

#[tokio::test]
async fn replaying_a_claim_returns_the_same_file_id() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());

    let original = seed_original(&repo).await;
    let key = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";

    let first = repo
        .create_reference(
            original.object.object_id,
            OTHER as u64,
            &ReferenceMetadata {
                original_filename: "mine.png",
                file_type: &FileType::Image,
                mime_type: "image/png",
                business_type: "message",
            },
            Some(key),
        )
        .await
        .expect("first claim");
    // 「响应丢了」= 客户端没收到结果，拿同一个 token 又来一次。
    let replay = repo
        .create_reference(
            original.object.object_id,
            OTHER as u64,
            &ReferenceMetadata {
                original_filename: "mine.png",
                file_type: &FileType::Image,
                mime_type: "image/png",
                business_type: "message",
            },
            Some(key),
        )
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

/// 🔴 收敛命中了一个已有对象，而那个对象在它拿到**对象锁**之前被删掉：
/// 必须退回用自己刚上传的那份，而不是把一个已经不存在的 object_id 交出去
/// （调用方拿它去插引用会撞 FK，一次本可成功的上传就此失败）。
///
/// 这条门禁盯的正是「内容锁与删除锁不是同一把」这个洞：内容锁只把同内容的首传
/// 串起来，删除和 `create_reference` 锁的是 `object_id`，两者互不同步。所以命中之后
/// 必须再取一次对象锁并复查。
///
/// 用两条连接把顺序钉死，不需要给生产函数开测试注入点：
///
///   1. A 先持有该对象的 advisory 锁；
///   2. B 调生产 `converge_upload`：内容锁拿得到、按明文摘要查得到那个对象，
///      然后**堵在**对象锁上；
///   3. A 删掉对象行并提交（锁随之释放）；
///   4. B 拿到锁，复查发现对象已经没了，退回自己那份。
#[tokio::test]
async fn convergence_falls_back_when_the_path_is_deleted_while_it_waits() {
    use privchat::service::file_service::{converge_upload, UploadPlacement};

    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());
    let original = seed_original(&repo).await;

    // 1. A 抢先占住对象锁。
    let object_id = original.object.object_id as i64;
    let mut tx_a = pool.begin().await.expect("tx a");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(object_id)
        .execute(&mut *tx_a)
        .await
        .expect("a holds object lock");

    // 2. B 去收敛：会查到那个对象，然后堵在对象锁上。
    let pool_b = pool.clone();
    let b = tokio::spawn(async move {
        let mut tx = pool_b.begin().await.expect("tx b");
        let placement = converge_upload(
            &mut tx,
            &UploadPlacement {
                plaintext_sha256: SHA.to_string(),
                plaintext_size: 1024,
                sealed_sha256: "5e".repeat(32),
                sealed_size: 1092,
                my_path: "/tmp/privchat-dedup-test/mine.bin".to_string(),
                my_source_id: 0,
                format_version: 1,
                encryption_key_id: 1,
            },
        )
        .await
        .expect("converge");
        tx.rollback().await.ok();
        placement
    });

    // 🔴 确认 B 确实**在等这把锁**，而不是靠睡一会儿再猜。
    //
    // 固定等待在慢机器上会失效：B 可能还没执行到查询，删除就发生了，
    // 这条测试于是退化成「先删再收敛」——走的是 `None` 分支，复查根本没被执行，
    // 也就成了假绿。这里直接问 Postgres：有没有人在等这把 advisory 锁。
    wait_until_blocked_on_advisory_lock(&pool, object_id).await;

    // 3. A 删掉最后一条引用连同对象行并提交，释放锁。
    sqlx::query("DELETE FROM privchat_file_uploads WHERE file_id = $1")
        .bind(original.file_id as i64)
        .execute(&mut *tx_a)
        .await
        .expect("delete last reference");
    sqlx::query("DELETE FROM privchat_attachment_objects WHERE object_id = $1")
        .bind(object_id)
        .execute(&mut *tx_a)
        .await
        .expect("delete the object");
    tx_a.commit().await.expect("commit a");

    // 4. B 拿到锁后复查失败，退回自己那份。
    let placement = b.await.expect("join b");
    assert_eq!(
        placement.file_path, "/tmp/privchat-dedup-test/mine.bin",
        "等锁期间对象被删了，必须退回自己刚上传的那份",
    );
    assert!(!placement.duplicate, "自己那份现在是唯一的一份，不能删");

    cleanup(&pool).await;
}



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
                plaintext_sha256: Some(SHA.to_string()),
                plaintext_size: Some(1024),
                declared_size: Some(1092),
                mime_type: Some("image/png".to_string()),
                format_version: Some(1),
                encryption_key_id: Some(1),
                chunk_plain_size: Some(
                    privchat_protocol::attachment_crypto::DEFAULT_CHUNK_PLAIN_SIZE,
                ),
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

    // 🔴 返回的必须是**新那一行**的真实数据，不是克隆源行改个 id。
    // 克隆出来的对象带着原上传者的归属和业务绑定；当前 RPC 恰好没下发这几个字段，
    // 所以看不出问题——但这个领域对象一旦被别处拿去判归属，错的就是权限。
    assert_eq!(
        first.uploader_id, OTHER as u64,
        "归属必须是取用者自己，不能是原上传者",
    );
    assert_eq!(
        first.business_id, None,
        "新行不继承源行的业务绑定：它要绑到取用者自己的那条消息上",
    );

    // 🔴 token **没有**被消费：一次性消费已经取消（一张 token、24 小时、可重复出示）。
    //
    // 这条断言原本写的是「第一次成功之后 token 应当已经作废」，那是旧模型的前提。
    // 现在它反过来是必须成立的事实——而且正因为如此，下面那次重放的正确性
    // **不能**再由「token 已作废」顺带解释：它只能来自幂等回查。
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        token_service
            .validate_any(now_secs, &token.token)
            .await
            .is_ok(),
        "取用成功不该让 token 失效：有效期内它一直可用",
    );

    // 「响应丢了」：客户端拿同一个 token 再来一次。
    let replay = claim_existing_file(&file_service, &token_service, OTHER as u64, &token.token, SHA)
        .await
        .expect("replayed claim must succeed and must not create a second row");
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




/// 按会话类型建一条引用这份文件、且 OTHER 有权读的消息，返回 message_id。
///
/// 三种类型的授权主体不同（频道行 / group_members / participants），并发用例要
/// 逐一覆盖，所以这里按类型分建而不是只建一种。
async fn seed_reference_for(pool: &sqlx::PgPool, channel_type: i16, file_id: u64) -> i64 {
    let channel: i64 = 972_000 + channel_type as i64;
    let msg: i64 = 972_100 + channel_type as i64;

    // 私聊频道对 (user1, user2) 有唯一索引，同一对用户只能有一条。别的 fixture
    // 可能已经建过，所以先找再建。
    let mut channel = channel;
    match channel_type {
        0 => {
            let existing: Option<(i64,)> = sqlx::query_as(
                "SELECT channel_id FROM privchat_channels
                  WHERE channel_type = 0
                    AND LEAST(direct_user1_id, direct_user2_id) = LEAST($1, $2)
                    AND GREATEST(direct_user1_id, direct_user2_id) = GREATEST($1, $2)
                  LIMIT 1",
            )
            .bind(OWNER)
            .bind(OTHER)
            .fetch_optional(pool)
            .await
            .expect("look up direct channel");
            match existing {
                Some((id,)) => channel = id,
                None => {
                    sqlx::query(
                        "INSERT INTO privchat_channels
                             (channel_id, channel_type, direct_user1_id, direct_user2_id)
                         VALUES ($1, 0, $2, $3)",
                    )
                    .bind(channel)
                    .bind(OWNER)
                    .bind(OTHER)
                    .execute(pool)
                    .await
                    .expect("direct channel");
                }
            }
        }
        1 => {
            sqlx::query(
                "INSERT INTO privchat_groups (group_id, name, owner_id, qr_key)
                 VALUES ($1, 'dedup-lock-group', $2, 'dlq972001')
                 ON CONFLICT (group_id) DO NOTHING",
            )
            .bind(channel)
            .bind(OWNER)
            .execute(pool)
            .await
            .expect("group");
            sqlx::query(
                "INSERT INTO privchat_channels (channel_id, channel_type, group_id)
                 VALUES ($1, 1, $1) ON CONFLICT (channel_id) DO NOTHING",
            )
            .bind(channel)
            .execute(pool)
            .await
            .expect("group channel");
            sqlx::query(
                "INSERT INTO privchat_group_members (group_id, user_id, role, joined_at, left_at)
                 VALUES ($1, $2, 2, now_millis(), NULL)
                 ON CONFLICT (group_id, user_id) DO UPDATE SET left_at = NULL",
            )
            .bind(channel)
            .bind(OTHER)
            .execute(pool)
            .await
            .expect("group member");
        }
        _ => {
            sqlx::query(
                "INSERT INTO privchat_channels (channel_id, channel_type)
                 VALUES ($1, $2) ON CONFLICT (channel_id) DO NOTHING",
            )
            .bind(channel)
            .bind(channel_type)
            .execute(pool)
            .await
            .expect("channel");
            sqlx::query(
                "INSERT INTO privchat_channel_participants
                     (channel_id, user_id, role, joined_at, left_at)
                 VALUES ($1, $2, 2, now_millis(), NULL)
                 ON CONFLICT (channel_id, user_id) DO UPDATE SET left_at = NULL",
            )
            .bind(channel)
            .bind(OTHER)
            .execute(pool)
            .await
            .expect("participant");
        }
    }

    // 先删后建：消息主键含 created_at 且默认 now()，ON CONFLICT 永远不命中，
    // 撤回过的旧行会一直留着并被引用 join 命中。
    sqlx::query("DELETE FROM privchat_message_file_refs WHERE message_id = $1")
        .bind(msg)
        .execute(pool)
        .await
        .expect("reset refs");
    sqlx::query("DELETE FROM privchat_messages WHERE message_id = $1")
        .bind(msg)
        .execute(pool)
        .await
        .expect("reset message");
    sqlx::query(
        "INSERT INTO privchat_messages (message_id, channel_id, sender_id, pts, message_type, content)
         VALUES ($1, $2, $3, 1, 1, '[image]')",
    )
    .bind(msg)
    .bind(channel)
    .bind(OWNER)
    .execute(pool)
    .await
    .expect("message");
    sqlx::query(
        "INSERT INTO privchat_message_file_refs
             (message_id, message_created_at, file_id, role, ordinal, created_at)
         SELECT $1, m.created_at, $2, 0, 0, m.created_at
         FROM privchat_messages m WHERE m.message_id = $1",
    )
    .bind(msg)
    .bind(file_id as i64)
    .execute(pool)
    .await
    .expect("file ref");

    msg
}








/// claim 与「撤回 / 退群」**互不阻塞** —— 这是新语义下必须钉住的那一面。
///
/// 🔴 这两条用例（私聊撤回、群聊退群）原本断言的是反面：并发撤回/退群必须被
/// claim 持有的消息行、频道行锁挡住。那套锁是旧判据的产物——当时 claim 要先核对
/// "取用者对源记录所在的消息/频道有没有访问权"，为了让这个判定不被并发改写，
/// 它得一路锁住 message / channel / group_members。
///
/// 判据换成"持有 token 冻结的明文摘要即可取用"之后，claim 根本不再读这些表，
/// 那些锁也就不该存在了。**把旧断言留着会反过来钉死一套已经废弃的跨域加锁**：
/// 谁哪天顺手把 message 行锁加回 claim 里，旧用例反而变绿。所以这里翻过来钉
/// 新方向——claim 不碰消息域，撤回/退群不必等它。
///
/// 撤回真正该拦住的是**下载**，那条门禁在 `attachment_authz_db_test`
/// （撤回后该引用不再授权，兄弟引用仍然授权）。
#[tokio::test]
async fn a_claim_does_not_lock_the_message_domain() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());
    let original = seed_original(&repo).await;
    let msg = seed_reference_for(&pool, 0, original.file_id).await;

    // claim 与撤回同时发生。
    let claim_pool = pool.clone();
    let source = original.clone();
    let claim = tokio::spawn(async move {
        FileUploadRepository::new(claim_pool)
            .create_reference(
                source.object.object_id,
                OTHER as u64,
                &ReferenceMetadata {
                    original_filename: "mine.png",
                    file_type: &FileType::Image,
                    mime_type: "image/png",
                    business_type: "message",
                },
                Some("no-cross-domain-lock"),
            )
            .await
    });

    // 🔴 撤回必须**自己走完**，不需要等 claim：两者之间没有锁耦合。
    // 有耦合的话这句会一直等到 claim 结束，超时就是红。
    let revoke = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sqlx::query("UPDATE privchat_messages SET revoked = true WHERE message_id = $1")
            .bind(msg)
            .execute(pool.as_ref()),
    )
    .await
    .expect("🔴 撤回被 claim 挡住了：claim 不该在消息域上加锁");
    revoke.expect("revoke");

    let mine = claim.await.expect("claim task").expect("claim 照常成立");
    assert_ne!(mine, original.file_id, "拿到的是自己的新引用");

    cleanup(&pool).await;
}



/// 先删后建一条消息并挂上文件引用（消息主键含 created_at，ON CONFLICT 不管用）。
async fn seed_message_ref(pool: &sqlx::PgPool, msg: i64, channel: i64, file_id: u64) {
    sqlx::query("DELETE FROM privchat_message_file_refs WHERE message_id = $1")
        .bind(msg)
        .execute(pool)
        .await
        .expect("reset refs");
    sqlx::query("DELETE FROM privchat_messages WHERE message_id = $1")
        .bind(msg)
        .execute(pool)
        .await
        .expect("reset message");
    sqlx::query(
        "INSERT INTO privchat_messages (message_id, channel_id, sender_id, pts, message_type, content)
         VALUES ($1, $2, $3, 1, 1, '[image]')",
    )
    .bind(msg)
    .bind(channel)
    .bind(OWNER)
    .execute(pool)
    .await
    .expect("message");
    sqlx::query(
        "INSERT INTO privchat_message_file_refs
             (message_id, message_created_at, file_id, role, ordinal, created_at)
         SELECT $1, m.created_at, $2, 0, 0, m.created_at
         FROM privchat_messages m WHERE m.message_id = $1",
    )
    .bind(msg)
    .bind(file_id as i64)
    .execute(pool)
    .await
    .expect("file ref");
}

/// 多条引用共享一个对象：删掉其中一条，按摘要 claim 仍命中同一个对象。
///
/// 🔴 这条替换掉了 `a_stale_first_candidate_falls_through_to_the_next_one`。
/// 那条用例守的是旧模型的"遍历同内容的候选**文件记录**、逐条判授权、失效就落到
/// 下一条"。新模型里根本没有候选列表：明文摘要唯一对应一个物理对象
/// （`plaintext_sha256` 上有唯一约束），所以"落到下一条"这件事不存在了，
/// 机械移植只会得到一条永远为真的空断言。
///
/// 换成守新模型真正的性质：引用与对象的生命周期是分开的——少一条引用不影响
/// 对象，也不影响后来者按摘要命中它。
#[tokio::test]
async fn dropping_one_reference_keeps_the_object_claimable() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());

    let original = seed_original(&repo).await;
    // 第二个用户取用同一份内容。
    let second = claim_for(&repo, original.object.object_id, OTHER as u64, "second.png", "image/png").await;

    // 删掉第一条引用（源行）。物理对象还被第二条引用着，必须留下。
    repo.delete(original.file_id).await.expect("delete the first reference");

    let object = repo
        .find_object_by_plaintext_sha256(SHA)
        .await
        .expect("probe")
        .expect("🔴 还有引用指着它，对象不该被删");
    assert_eq!(object.object_id, original.object.object_id, "还是同一个物理对象");

    // 第三个用户仍能按摘要命中它，并拿到**自己**的元数据。
    const THIRD: i64 = 9_980_031;
    sqlx::query(
        "INSERT INTO privchat_users (user_id, username, display_name, qr_key) \
         VALUES ($1, 'dedup_third', 'dedup_third', $2) ON CONFLICT (user_id) DO NOTHING",
    )
    .bind(THIRD)
    .bind(privchat::rpc::qr::generate_qr_key())
    .execute(pool.as_ref())
    .await
    .expect("third user");
    let third = claim_for(&repo, object.object_id, THIRD as u64, "third.png", "image/jpeg").await;
    assert_ne!(third, second, "各人各一条引用");

    let (filename, mime): (String, String) = sqlx::query_as(
        "SELECT original_filename, mime_type FROM privchat_file_uploads WHERE file_id = $1",
    )
    .bind(third as i64)
    .fetch_one(pool.as_ref())
    .await
    .expect("read third row");
    assert_eq!(filename, "third.png", "元数据来自取用者自己的 token");
    assert_eq!(mime, "image/jpeg");

    // 全程只有一个物理对象。
    let objects: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM privchat_attachment_objects WHERE plaintext_sha256 = $1",
    )
    .bind(SHA)
    .fetch_one(pool.as_ref())
    .await
    .expect("count objects");
    assert_eq!(objects, 1, "三条引用，一个物理对象");

    cleanup(&pool).await;
}

/// 同一份内容有多条记录时，按**请求者能读哪一条**授权，不是钉死最老那条。
///
/// 🔴 alice 发在群 A（较老的 file_id），bob 发在群 B（较新的）。charlie 只在群 B。
/// `find_by_content` 按 `ORDER BY file_id` 返回最老那条，如果授权也照着它判，
/// charlie 会被拒——而他明明有权拿到这份内容。物理文件是同一个，授权按记录算。
///
/// 走真实的 `claim_existing_file`（prepare → find_by_content → 授权 → copy），
/// 只调 `copy_for_user` 是抓不到这个的：那样等于替服务端选好了源。
#[tokio::test]
async fn a_claimer_is_authorized_against_the_record_they_can_actually_read() {
    use privchat::service::file_claim_service::claim_existing_file;
    use privchat::service::upload_token_service::{
        UploadIdentity, UploadTokenPurpose, UploadTokenService,
    };
    use privchat::service::FileService;

    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());

    // 较老的一条：OWNER 上传，挂在 charlie 读不到的地方（不建任何引用即可——
    // 无引用时只有上传者可读）。
    let older = seed_original(&repo).await;
    // 较新的一条：同 hash、同物理路径，挂在 charlie 在的群里。
    let mut newer = older.clone();
    newer.file_id = repo.next_file_id().await.expect("file id");
    newer.uploader_id = OWNER as u64;
    repo.insert(&newer).await.expect("insert newer record");
    assert!(newer.file_id > older.file_id, "较新那条的 file_id 必须更大");

    const CHARLIE: i64 = 9_980_021;
    sqlx::query(
        "INSERT INTO privchat_users (user_id, username, display_name, qr_key)
         VALUES ($1, 'dd_charlie', 'dd_charlie', 'dgc9980021')
         ON CONFLICT (user_id) DO NOTHING",
    )
    .bind(CHARLIE)
    .execute(pool.as_ref())
    .await
    .expect("charlie");
    const GROUP: i64 = 975_001;
    const MSG: i64 = 975_002;
    sqlx::query(
        "INSERT INTO privchat_groups (group_id, name, owner_id, qr_key)
         VALUES ($1, 'dedup-pick', $2, 'dpq975001') ON CONFLICT (group_id) DO NOTHING",
    )
    .bind(GROUP)
    .bind(OWNER)
    .execute(pool.as_ref())
    .await
    .expect("group");
    sqlx::query(
        "INSERT INTO privchat_channels (channel_id, channel_type, group_id)
         VALUES ($1, 1, $1) ON CONFLICT (channel_id) DO NOTHING",
    )
    .bind(GROUP)
    .execute(pool.as_ref())
    .await
    .expect("group channel");
    sqlx::query(
        "INSERT INTO privchat_group_members (group_id, user_id, role, joined_at, left_at)
         VALUES ($1, $2, 2, now_millis(), NULL)
         ON CONFLICT (group_id, user_id) DO UPDATE SET left_at = NULL",
    )
    .bind(GROUP)
    .bind(CHARLIE)
    .execute(pool.as_ref())
    .await
    .expect("charlie joins");
    // 生产入群会同时写 participants（channel_repo.rs:409/631），规范判据
    // `resolve_attachment_access` 的成员部分读的正是这张表。只写 group_members
    // 的话，这里测的就不是「选对记录」，而是「fixture 少了一半」。
    sqlx::query(
        "INSERT INTO privchat_channel_participants (channel_id, user_id, role, joined_at, left_at)
         VALUES ($1, $2, 2, now_millis(), NULL)
         ON CONFLICT (channel_id, user_id) DO UPDATE SET left_at = NULL",
    )
    .bind(GROUP)
    .bind(CHARLIE)
    .execute(pool.as_ref())
    .await
    .expect("charlie participant row");
    seed_message_ref(&pool, MSG, GROUP, newer.file_id).await;

    let file_service = Arc::new(FileService::new(Vec::new(), 0, pool.clone()));
    let token_service = Arc::new(UploadTokenService::new());
    let token = token_service
        .generate_token(
            CHARLIE as u64,
            FileType::Image,
            10 * 1024 * 1024,
            "message".to_string(),
            None,
            UploadIdentity {
                plaintext_sha256: Some(SHA.to_string()),
                plaintext_size: Some(1024),
                declared_size: Some(1092),
                mime_type: Some("image/png".to_string()),
                format_version: Some(1),
                encryption_key_id: Some(1),
                chunk_plain_size: Some(
                    privchat_protocol::attachment_crypto::DEFAULT_CHUNK_PLAIN_SIZE,
                ),
            },
            UploadTokenPurpose::ClaimExisting,
        )
        .await
        .expect("token");

    let claimed = claim_existing_file(&file_service, &token_service, CHARLIE as u64,
        &token.token,
        SHA,
    )
    .await;

    let claimed = claimed.expect(
        "🔴 charlie 有权读较新那条记录，不能因为 find_by_content 先返回最老那条就拒绝",
    );
    assert_eq!(claimed.uploader_id, CHARLIE as u64, "拿到的是自己的记录");
    assert_eq!(claimed.file_path(), older.file_path(), "指向同一个物理文件");

    sqlx::query("DELETE FROM privchat_file_uploads WHERE uploader_id = $1")
        .bind(CHARLIE)
        .execute(pool.as_ref())
        .await
        .expect("clean charlie rows");
    cleanup(&pool).await;
}

/// 锁等待超时必须是**可重试**的，不是终局失败。
///
/// 🔴 占的必须是**生产真正在等的那把**：`create_reference` 锁的是 `object_id`。
/// 这条用例原本占 `hashtext(file_path)`——那把锁生产早就不用了，于是 claim 一路
/// 畅通返回 Ok，用例"通过"证明的是它自己没挡住任何东西。
#[tokio::test]
async fn a_lock_timeout_is_retryable_not_terminal() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());
    let original = seed_original(&repo).await;

    // 别人占着同一把对象锁不放。
    let mut squatter = pool.begin().await.expect("squatter tx");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(original.object.object_id as i64)
        .execute(&mut *squatter)
        .await
        .expect("squat the file lock");

    let result = repo
        .create_reference(original.object.object_id, OTHER as u64, &ReferenceMetadata {
            original_filename: &original.original_filename,
            file_type: &original.file_type,
            mime_type: &original.mime_type,
            business_type: "message",
        }, None)
        .await;
    squatter.rollback().await.ok();

    match result {
        Err(privchat::error::ServerError::ServiceUnavailable(_)) => {}
        other => panic!("🔴 锁等待超时必须映射成可重试的 ServiceUnavailable，实际: {other:?}"),
    }

    cleanup(&pool).await;
}

/// 轮询 `pg_locks`，直到确实有连接在等这把 advisory 锁。
///
/// 判据是「有一条 advisory 锁记录处于未授予状态」——不是睡够时间，
/// 所以机器再慢也不会提前往下走。
async fn wait_until_blocked_on_advisory_lock(pool: &sqlx::PgPool, key: i64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks \
             WHERE locktype = 'advisory' AND NOT granted \
               AND ((classid::bigint << 32) | objid::bigint) = $1",
        )
        .bind(key)
        .fetch_one(pool)
        .await
        .expect("read pg_locks");
        if waiting > 0 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "10 秒内没有观察到有人在等这把 advisory 锁；\
             不能继续，否则删除会落在错误的时点上，测试变成假绿",
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// **持有 token 冻结的明文摘要即可 claim** —— 这是冻结下来的语义，这条用例钉住它。
///
/// 🔴 方向与上一版相反，不是放松了要求，而是换了判据。上一版要求「取用者对源记录
/// 所在的消息/频道有访问权」，那条规则让**跨用户秒传根本不成立**：两个互不相识的人
/// 发同一份文件时，第二个人对第一个人的记录当然没有访问权，于是每次都退回整传，
/// 秒传的收益（几乎全在"别人已经传过"）一分都拿不到。
///
/// 新判据下能拿到的只是「一条指向同一物理对象的**自己的**引用」。撤回、退群影响的是
/// **原引用的下载授权**（门禁在 `attachment_authz_db_test`），不影响新引用的创建；
/// 而想 claim 就得先有明文摘要——它由 token 冻结，不是随便猜得出来的。
#[tokio::test]
async fn holding_the_frozen_digest_is_enough_to_claim() {
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

    // 关键：**不**建立任何引用/成员关系。OTHER 只是知道那串摘要而已。
    let file_service = Arc::new(FileService::new(Vec::new(), 0, pool.clone()));
    let token_service = Arc::new(UploadTokenService::new());
    let token = token_service
        .generate_token(
            OTHER as u64,
            FileType::Image,
            10 * 1024 * 1024,
            "message".to_string(),
            None,
            UploadIdentity {
                plaintext_sha256: Some(SHA.to_string()),
                plaintext_size: Some(1024),
                declared_size: Some(1092),
                mime_type: Some("image/png".to_string()),
                format_version: Some(1),
                encryption_key_id: Some(1),
                chunk_plain_size: Some(
                    privchat_protocol::attachment_crypto::DEFAULT_CHUNK_PLAIN_SIZE,
                ),
            },
            UploadTokenPurpose::ClaimExisting,
        )
        .await
        .expect("token");

    let refused = claim_existing_file(&file_service, &token_service, OTHER as u64,
        &token.token,
        SHA,
    )
    .await;

    let mine = refused.expect("持有冻结摘要即可取用");
    assert_ne!(mine.file_id, original.file_id, "拿到的是自己的新引用");
    assert_eq!(mine.uploader_id, OTHER as u64, "新引用归属取用者自己");
    assert_eq!(
        mine.object.object_id, original.object.object_id,
        "指向同一个物理对象——这才是秒传"
    );

    // 幂等键落库：同一张 token 再来一次会回到同一个 file_id。
    let claimed = repo
        .find_claimed(OTHER as u64, &privchat::service::file_claim_service::claim_key_hash(&token.token))
        .await
        .expect("query claimed");
    assert_eq!(claimed, Some(mine.file_id), "claim 必须记下幂等键");

    cleanup(&pool).await;
}

/// 私聊没有 participants 行、群聊成员在 group_members —— 都必须放行。
///
/// 🔴 只查 `privchat_channel_participants` 会把这两种**合法**取用挡在外面。
/// 权威来源按会话类型分流（与投递收件人那份表达式同形）：
///   Direct(0) → 频道行的 direct_user1/2_id；Group(1) → group_members；其它 → participants。
///
/// 之前的测试自己补了 participants 行，正好把这个 bug 盖住了——所以这里刻意
/// **不**建 participants。
#[tokio::test]
async fn direct_and_group_members_are_authorized_without_participant_rows() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());

    const DM: i64 = 971_001;
    const GROUP: i64 = 971_002;
    const DM_MSG: i64 = 971_003;
    const GROUP_MSG: i64 = 971_004;

    for (uid, name) in [
        (DIRECT_PEER, "dd_peer"),
        (DIRECT_HOST, "dd_owner"),
    ] {
        sqlx::query(
            "INSERT INTO privchat_users (user_id, username, display_name, qr_key)
             VALUES ($1, $2, $2, $3) ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(uid)
        .bind(name)
        .bind(format!("dg{uid}"))
        .execute(pool.as_ref())
        .await
        .expect("ensure user");
    }

    // ---- 私聊：成员写在频道行上，**没有 participants 行** ----
    sqlx::query(
        "INSERT INTO privchat_channels (channel_id, channel_type, direct_user1_id, direct_user2_id)
         VALUES ($1, 0, $2, $3) ON CONFLICT (channel_id) DO NOTHING",
    )
    .bind(DM)
    .bind(DIRECT_HOST)
    .bind(DIRECT_PEER)
    .execute(pool.as_ref())
    .await
    .expect("direct channel");

    // ---- 群聊：成员在 group_members，同样没有 participants 行 ----
    sqlx::query(
        "INSERT INTO privchat_groups (group_id, name, owner_id, qr_key)
         VALUES ($1, 'dedup-guard-group', $2, 'dgq971002') ON CONFLICT (group_id) DO NOTHING",
    )
    .bind(GROUP)
    .bind(DIRECT_HOST)
    .execute(pool.as_ref())
    .await
    .expect("group");
    sqlx::query(
        "INSERT INTO privchat_channels (channel_id, channel_type, group_id)
         VALUES ($1, 1, $1) ON CONFLICT (channel_id) DO NOTHING",
    )
    .bind(GROUP)
    .execute(pool.as_ref())
    .await
    .expect("group channel");
    sqlx::query(
        "INSERT INTO privchat_group_members (group_id, user_id, role, joined_at, left_at)
         VALUES ($1, $2, 2, now_millis(), NULL)
         ON CONFLICT (group_id, user_id) DO UPDATE SET left_at = NULL",
    )
    .bind(GROUP)
    .bind(DIRECT_PEER)
    .execute(pool.as_ref())
    .await
    .expect("group member");

    for (msg, channel) in [(DM_MSG, DM), (GROUP_MSG, GROUP)] {
        let original = seed_original(&repo).await;
        sqlx::query("DELETE FROM privchat_message_file_refs WHERE message_id = $1")
            .bind(msg)
            .execute(pool.as_ref())
            .await
            .expect("reset refs");
        sqlx::query("DELETE FROM privchat_messages WHERE message_id = $1")
            .bind(msg)
            .execute(pool.as_ref())
            .await
            .expect("reset message");
        sqlx::query(
            "INSERT INTO privchat_messages (message_id, channel_id, sender_id, pts, message_type, content)
             VALUES ($1, $2, $3, 1, 1, '[image]')",
        )
        .bind(msg)
        .bind(channel)
        .bind(DIRECT_HOST)
        .execute(pool.as_ref())
        .await
        .expect("message");
        sqlx::query(
            "INSERT INTO privchat_message_file_refs
                 (message_id, message_created_at, file_id, role, ordinal, created_at)
             SELECT $1, m.created_at, $2, 0, 0, m.created_at
             FROM privchat_messages m WHERE m.message_id = $1",
        )
        .bind(msg)
        .bind(original.file_id as i64)
        .execute(pool.as_ref())
        .await
        .expect("file ref");

        let claimed = repo
            .create_reference(original.object.object_id, DIRECT_PEER as u64, &ReferenceMetadata {
            original_filename: &original.original_filename,
            file_type: &original.file_type,
            mime_type: &original.mime_type,
            business_type: "message",
        }, None)
            .await;
        assert!(
            claimed.is_ok(),
            "channel_id={channel} 的合法成员被误拒了：{claimed:?}"
        );
        cleanup(&pool).await;
    }
}

/// 撤回之后再 claim：**照常成立**，而且这条规则是有意的。
///
/// 🔴 这条用例原本断言的是反面（"撤回之后不能再开出新的 file_id"）。那个判据
/// 已经随跨用户秒传一起去掉了，理由写在 `create_reference` 的文档里：claim 的
/// 授权依据是**持有明文 SHA-256**，不是"对源记录所在的消息/频道有访问权"。
/// 保留旧判据的话，两个互不相识的人发同一份文件时，第二个人对第一个人的记录
/// 当然没有访问权——跨用户秒传于是根本不成立，每次都退回整传。
///
/// 我没有把它悄悄删掉：语义换了方向的用例，改成钉住**新方向**才拦得住"哪天有人
/// 又把授权复查加回 claim 里"。撤回该拦住的是**下载**，那条门禁在
/// `attachment_authz_db_test`（撤回后引用不再授权，兄弟引用仍然授权）。
#[tokio::test]
async fn a_claim_still_succeeds_after_the_source_message_is_revoked() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());
    let original = seed_original(&repo).await;

    sqlx::query("UPDATE privchat_messages SET revoked = true WHERE message_id = 970002")
        .execute(pool.as_ref())
        .await
        .expect("revoke");

    let mine = repo
        .create_reference(
            original.object.object_id,
            OTHER as u64,
            &ReferenceMetadata {
                original_filename: "mine.png",
                file_type: &FileType::Image,
                mime_type: "image/png",
                business_type: "message",
            },
            Some("race-key"),
        )
        .await
        .expect("持有明文摘要即可取用，与源消息是否被撤回无关");
    assert_ne!(mine, original.file_id, "拿到的是自己的新引用");

    // 两条引用指向同一个物理对象：秒传的意义就在这里。
    assert!(
        repo.other_rows_share_object(mine, original.object.object_id)
            .await
            .expect("count"),
        "新引用必须指向既有对象，而不是另建一份",
    );

    cleanup(&pool).await;
}

/// purpose 双向隔离：两种 token 各自只能走自己那条入口。
///
/// 🔴 一次性 token 只挡得住「同一入口用两次」。没有用途隔离的话，两个入口
/// 并发各用一次，会同时留下一条 claim 行和一条上传行。
#[tokio::test]
async fn a_token_can_only_be_used_for_its_own_purpose() {
    use privchat::service::file_claim_service::claim_existing_file;
    use privchat::service::upload_token_service::{
        UploadIdentity, UploadTokenPurpose, UploadTokenService,
    };
    use privchat::service::FileService;

    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());
    // 库里得有这份内容，purpose 隔离才谈得上"本来能成"。
    let _original = seed_original(&repo).await;

    let file_service = Arc::new(FileService::new(Vec::new(), 0, pool.clone()));
    let token_service = Arc::new(UploadTokenService::new());

    let identity = || UploadIdentity {
        plaintext_sha256: Some(SHA.to_string()),
        plaintext_size: Some(1024),
        declared_size: Some(1092),
        mime_type: Some("image/png".to_string()),
        format_version: Some(1),
        encryption_key_id: Some(1),
        chunk_plain_size: Some(privchat_protocol::attachment_crypto::DEFAULT_CHUNK_PLAIN_SIZE),
    };

    // 实体上传用途的 token 拿去 claim → 拒绝。
    let upload_token = token_service
        .generate_token(
            OTHER as u64,
            FileType::Image,
            10 * 1024 * 1024,
            "message".to_string(),
            None,
            identity(),
            UploadTokenPurpose::Upload,
        )
        .await
        .expect("upload token");
    let refused = claim_existing_file(&file_service, &token_service, OTHER as u64,
        &upload_token.token,
        SHA,
    )
    .await;
    assert!(
        refused.is_err(),
        "实体上传用途的 token 不能拿去秒传取用——否则同一张 token 能在两个入口各用一次",
    );

    // 反向：claim 用途的 token 拿去实体上传 → 也要拒绝。
    // 上传入口的判据与这里同构（`purpose != Upload` 即拒），这里锁住这个枚举语义。
    let claim_token = token_service
        .generate_token(
            OTHER as u64,
            FileType::Image,
            10 * 1024 * 1024,
            "message".to_string(),
            None,
            identity(),
            UploadTokenPurpose::ClaimExisting,
        )
        .await
        .expect("claim token");
    assert_ne!(
        claim_token.purpose,
        UploadTokenPurpose::Upload,
        "claim 用途的 token 在实体上传入口必须不等于 Upload，从而被拒",
    );

    // 用途正确时照常放行。
    assert!(claim_existing_file(&file_service, &token_service, OTHER as u64,
        &claim_token.token,
        SHA,
    )
    .await
    .is_ok());

    cleanup(&pool).await;
}

/// 🔴 **内容锁**的等待上限与错误分类。
///
/// 这条盯的是两件事，缺一条都会让瞬时竞争变成终局失败：
///
///   1. `lock_timeout` 必须设在**第一把锁之前**。它曾经设在"命中已有对象之后、
///      取对象锁之前"——而同内容首传的竞争恰恰全发生在内容锁上，那个 3 秒上限
///      对它一次都不生效，一个卡住的事务能把上传挂到天荒地老。
///   2. 超时必须映射成**可重试**的 `ServiceUnavailable`，与 `create_reference`
///      同一处映射。包成 `Database` 就是终局失败，而且同一类超时会在不同上传
///      路径上表现成不同错误。
#[tokio::test]
async fn a_content_lock_timeout_is_retryable_not_terminal() {
    use privchat::service::file_service::{converge_upload, UploadPlacement};

    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;

    // 别人占着同一份内容的那把锁不放（对象还不存在，收敛必然堵在第一把锁上）。
    let mut squatter = pool.begin().await.expect("squatter tx");
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(SHA)
        .execute(&mut *squatter)
        .await
        .expect("squat the content lock");

    let mut tx = pool.begin().await.expect("tx");
    let started = std::time::Instant::now();
    let result = converge_upload(
        &mut tx,
        &UploadPlacement {
            plaintext_sha256: SHA.to_string(),
            plaintext_size: 1024,
            sealed_sha256: "5e".repeat(32),
            sealed_size: 1092,
            my_path: "/tmp/privchat-dedup-test/timeout.bin".to_string(),
            my_source_id: 0,
            format_version: 1,
            encryption_key_id: 1,
        },
    )
    .await;
    let waited = started.elapsed();
    tx.rollback().await.ok();
    squatter.rollback().await.ok();

    assert!(
        matches!(result, Err(privchat::error::ServerError::ServiceUnavailable(_))),
        "锁等待超时必须是可重试的 ServiceUnavailable，实际: {result:?}"
    );
    assert!(
        waited < std::time::Duration::from_secs(10),
        "🔴 上限没生效：等了 {waited:?}——`lock_timeout` 必须设在第一把锁之前"
    );

    cleanup(&pool).await;
}
