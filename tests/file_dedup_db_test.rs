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
        .bind(&vec![OWNER, OTHER, DIRECT_PEER, DIRECT_HOST])
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
    authorize_claimer(&pool, original.file_id, original.uploader_id as i64, OTHER).await;

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
    authorize_claimer(&pool, original.file_id, original.uploader_id as i64, OTHER).await;
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
    authorize_claimer(&pool, original.file_id, original.uploader_id as i64, OTHER).await;
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
    authorize_claimer(&pool, original.file_id, original.uploader_id as i64, OTHER).await;
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
    authorize_claimer(&pool, original.file_id, original.uploader_id as i64, OTHER).await;

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

    // 🔴 确认 B 确实**在等这把锁**，而不是靠睡一会儿再猜。
    //
    // 固定等待在慢机器上会失效：B 可能还没执行到查询，删除就发生了，
    // 这条测试于是退化成「先删再收敛」——走的是 `None` 分支，复查根本没被执行，
    // 也就成了假绿。这里直接问 Postgres：有没有人在等这把 advisory 锁。
    wait_until_blocked_on_advisory_lock(&pool, SHARED_PATH).await;

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
/// 让 `claimer` 对源文件**确实有读取权限**：把文件挂到一条双方都在的会话消息上。
///
/// 不建这个前提的话，claim 会被授权闸门拒掉——那正是它该做的事，但这两条用例
/// 要验的是 token 用途和重放幂等，不是授权本身（授权在 `attachment_authz_db_test`）。
async fn authorize_claimer(pool: &sqlx::PgPool, file_id: u64, uploader: i64, claimer: i64) {
    const CH: i64 = 970_001;
    const MSG: i64 = 970_002;
    let qr = |n: i64| format!("dedup-qr-{n}");
    for (uid, name) in [(uploader, "dedup_uploader"), (claimer, "dedup_claimer")] {
        sqlx::query(
            "INSERT INTO privchat_users (user_id, username, display_name, qr_key)
             VALUES ($1, $2, $2, $3) ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(uid)
        .bind(name)
        .bind(qr(uid))
        .execute(pool)
        .await
        .expect("ensure user");
    }
    sqlx::query(
        "INSERT INTO privchat_channels (channel_id, channel_type, direct_user1_id, direct_user2_id)
         VALUES ($1, 0, $2, $3) ON CONFLICT (channel_id) DO NOTHING",
    )
    .bind(CH)
    .bind(uploader)
    .bind(claimer)
    .execute(pool)
    .await
    .expect("ensure channel");
    for uid in [uploader, claimer] {
        sqlx::query(
            "INSERT INTO privchat_channel_participants (channel_id, user_id, role, joined_at, left_at)
             VALUES ($1, $2, 2, now_millis(), NULL)
             ON CONFLICT (channel_id, user_id) DO UPDATE SET left_at = NULL",
        )
        .bind(CH)
        .bind(uid)
        .execute(pool)
        .await
        .expect("ensure participant");
    }
    // 🔴 先删后建，别指望 ON CONFLICT。
    //
    // `privchat_messages` 的主键含 `created_at`，而它默认取 now()——所以每跑一次
    // 就多一行同 message_id 的消息，ON CONFLICT 永远不命中。撤回过的那些旧行
    // 一直留着，引用 join 照样命中它们，于是一堆与撤回无关的用例集体被闸门拒掉。
    sqlx::query("DELETE FROM privchat_messages WHERE message_id = $1")
        .bind(MSG)
        .execute(pool)
        .await
        .expect("reset message");
    sqlx::query(
        "INSERT INTO privchat_messages (message_id, channel_id, sender_id, pts, message_type, content)
         VALUES ($1, $2, $3, 1, 1, '[image]')",
    )
    .bind(MSG)
    .bind(CH)
    .bind(uploader)
    .execute(pool)
    .await
    .expect("ensure message");
    // 🔴 先删。引用表主键不含 file_id，上一轮跑剩的行会让 ON CONFLICT DO NOTHING
    // 静默吞掉这次插入——于是文件看起来「从未被引用」，授权回落到只有上传者可读，
    // 表现为一条与授权无关的用例莫名其妙失败。
    sqlx::query("DELETE FROM privchat_message_file_refs WHERE message_id = $1")
        .bind(MSG)
        .execute(pool)
        .await
        .expect("reset file refs");
    sqlx::query(
        "INSERT INTO privchat_message_file_refs
             (message_id, message_created_at, file_id, role, ordinal, created_at)
         SELECT $1, m.created_at, $2, 0, 0, m.created_at
         FROM privchat_messages m WHERE m.message_id = $1
         ON CONFLICT DO NOTHING",
    )
    .bind(MSG)
    .bind(file_id as i64)
    .execute(pool)
    .await
    .expect("insert file ref");
}

/// 秒传取用的授权判据与 `file/get_url` 同一套，所以测试也得把那两样依赖备齐。
fn authorization_deps(
    pool: &sqlx::PgPool,
) -> (
    privchat::repository::message_repo::PgMessageRepository,
    privchat::service::channel_service::ChannelService,
) {
    (
        privchat::repository::message_repo::PgMessageRepository::new(std::sync::Arc::new(
            pool.clone(),
        )),
        privchat::service::channel_service::ChannelService::new_with_repository(std::sync::Arc::new(
            privchat::repository::channel_repo::PgChannelRepository::new(std::sync::Arc::new(
                pool.clone(),
            )),
        )),
    )
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
    authorize_claimer(&pool, original.file_id, original.uploader_id as i64, OTHER).await;

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

    authorize_claimer(&pool, original.file_id, original.uploader_id as i64, OTHER).await;
    let (messages, channels) = authorization_deps(&pool);
    let first = claim_existing_file(&file_service, &token_service, &messages, &channels, OTHER as u64, &token.token, SHA)
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

    // token 此刻已被消费——这正是重放路径的前提。
    assert!(
        token_service.validate_token(&token.token).await.is_err(),
        "第一次成功之后 token 应当已经作废",
    );

    // 「响应丢了」：客户端拿同一个 token 再来一次。
    let replay = claim_existing_file(&file_service, &token_service, &messages, &channels, OTHER as u64, &token.token, SHA)
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

/// 轮询 `pg_locks`，直到确实有连接在等**行**锁（tuple / transactionid）。
///
/// 判据同样是「有一条未授予的锁记录」而不是睡够时间：撤回被 claim 的共享锁挡住
/// 时，等待就体现在这里。
async fn wait_until_blocked_on_row_lock(pool: &sqlx::PgPool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks \
             WHERE NOT granted AND locktype IN ('tuple', 'transactionid')",
        )
        .fetch_one(pool)
        .await
        .expect("read pg_locks");
        if waiting > 0 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "10 秒内没有观察到有人在等行锁；不能继续，否则并发断言变成假绿",
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 撤回 / 退群必须等在**生产代码**持有的共享锁上。
///
/// 🔴 这里有两个必须做对的地方，之前各栽过一次：
///
/// 1. 必须跑**真实的** `copy_for_user`。测试自己写一条 `FOR SHARE` 只能证明
///    PostgreSQL 的锁语义，删掉生产锁照样绿。
/// 2. 必须确认「挡住我的就是那个 claim」。只看「我在等某个事务」不够——被别的
///    事务挡住也满足，同样是假绿。`pg_blocking_pids()` 直接回答这个问题，而且
///    不需要知道锁具体在哪一行。
struct InsertBarrier {
    pool: std::sync::Arc<sqlx::PgPool>,
}

impl InsertBarrier {
    /// 装一个 BEFORE INSERT trigger，让写 `privchat_file_uploads` 的事务停在
    /// 指定的 advisory 锁上——那一刻它的授权锁已经全部持住。
    async fn arm(pool: &std::sync::Arc<sqlx::PgPool>, key: i64) -> Self {
        // 先清残留：上一次跑如果在断言处 panic，trigger 会留在库里挡住所有
        // 后续 INSERT。
        Self::drop_all(pool).await;
        sqlx::raw_sql(&format!(
            "CREATE OR REPLACE FUNCTION dedup_claim_barrier() RETURNS trigger AS $$
             BEGIN
               PERFORM pg_advisory_xact_lock({key});
               RETURN NEW;
             END; $$ LANGUAGE plpgsql;
             CREATE TRIGGER dedup_claim_barrier BEFORE INSERT ON privchat_file_uploads
               FOR EACH ROW EXECUTE FUNCTION dedup_claim_barrier();"
        ))
        .execute(pool.as_ref())
        .await
        .expect("arm insert barrier");
        Self { pool: pool.clone() }
    }

    async fn drop_all(pool: &std::sync::Arc<sqlx::PgPool>) {
        sqlx::raw_sql(
            "DROP TRIGGER IF EXISTS dedup_claim_barrier ON privchat_file_uploads;
             DROP FUNCTION IF EXISTS dedup_claim_barrier();",
        )
        .execute(pool.as_ref())
        .await
        .expect("drop insert barrier");
    }
}

impl Drop for InsertBarrier {
    /// 🔴 断言 panic 时也要拆掉。留在库里的话，后面每一次文件 INSERT 都会挂在
    /// 那把 advisory 锁上——一条测试失败会连累整个套件。
    fn drop(&mut self) {
        let pool = self.pool.clone();
        // Drop 里不能 await：交给一个独立线程上的运行时同步跑完。
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("cleanup runtime");
            rt.block_on(async { InsertBarrier::drop_all(&pool).await });
        })
        .join();
    }
}

/// 等到 `waiter` 被挡住，并且**挡住它的正是** `blocker`。
async fn wait_until_blocked_by(pool: &sqlx::PgPool, waiter: i32, blocker: i32) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let blockers: Vec<i32> = sqlx::query_scalar("SELECT unnest(pg_blocking_pids($1))")
            .bind(waiter)
            .fetch_all(pool)
            .await
            .expect("pg_blocking_pids");
        if blockers.contains(&blocker) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "10 秒内 pid={waiter} 没有被 claim(pid={blocker}) 挡住；实际挡它的是 {blockers:?}",
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 等真实 claim 停在 barrier 上，返回它的后端 pid。
async fn wait_for_claim_at_barrier(pool: &sqlx::PgPool, key: i64) -> i32 {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let pid: Option<(i32,)> = sqlx::query_as(
            "SELECT pid FROM pg_locks
              WHERE locktype = 'advisory' AND NOT granted
                AND objid = ($1::bigint & 4294967295) LIMIT 1",
        )
        .bind(key)
        .fetch_optional(pool)
        .await
        .expect("find claim pid");
        if let Some((pid,)) = pid {
            return pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "10 秒内真实 claim 没有停在 barrier 上"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 报出自己的后端 pid，然后执行一条会被挡住的写操作。
async fn spawn_blocked_write(
    pool: std::sync::Arc<sqlx::PgPool>,
    sql: &'static str,
    a: i64,
    b: i64,
) -> (i32, tokio::task::JoinHandle<()>) {
    let (tx, rx) = tokio::sync::oneshot::channel::<i32>();
    let handle = tokio::spawn(async move {
        let mut conn = pool.acquire().await.expect("conn");
        let pid: (i32,) = sqlx::query_as("SELECT pg_backend_pid()")
            .fetch_one(&mut *conn)
            .await
            .expect("pid");
        tx.send(pid.0).expect("report pid");
        sqlx::query(sql)
            .bind(a)
            .bind(b)
            .execute(&mut *conn)
            .await
            .expect("write completes once unblocked");
    });
    (rx.await.expect("pid"), handle)
}

/// 私聊：并发撤回必须被真实 claim 持有的**消息行**锁挡住。
#[tokio::test]
async fn a_revoke_is_blocked_by_the_production_claim() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());
    let original = seed_original(&repo).await;
    let msg = seed_reference_for(&pool, 0, original.file_id).await;

    const BARRIER: i64 = 973_001;
    let _barrier = InsertBarrier::arm(&pool, BARRIER).await;
    let mut holder = pool.begin().await.expect("holder tx");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(BARRIER)
        .execute(&mut *holder)
        .await
        .expect("hold barrier");

    let claim_pool = pool.clone();
    let source = original.clone();
    let claim = tokio::spawn(async move {
        FileUploadRepository::new(claim_pool)
            .copy_for_user(&source, OTHER as u64, "message", None)
            .await
    });
    let claim_pid = wait_for_claim_at_barrier(&pool, BARRIER).await;

    let (revoke_pid, revoker) = spawn_blocked_write(
        pool.clone(),
        "UPDATE privchat_messages SET revoked = true WHERE message_id = $1 AND $2 = $2",
        msg,
        0,
    )
    .await;
    wait_until_blocked_by(&pool, revoke_pid, claim_pid).await;

    holder.commit().await.expect("release barrier");
    claim.await.expect("claim task").expect("claim succeeds");
    revoker.await.expect("revoke task");

    cleanup(&pool).await;
}

/// 群聊：并发退群必须被真实 claim 挡住。
///
/// ⚠️ 它守住的是**频道行锁**这条链路，不是成员行锁那一句。退群会触发
/// `trg_privchat_group_membership_version` 去更新 `privchat_channels.membership_version`，
/// 而那一行已经被 claim 的 `FOR SHARE OF m, c` 锁住——所以删掉生产里的
/// `privchat_group_members ... FOR SHARE`，这条依旧绿。实测确认过。
///
/// 成员行锁因此没有独立门禁；保留它的理由写在 `copy_for_user` 里（串行化不该
/// 依赖一个为同步版本号而加的 trigger 恰好存在）。
#[tokio::test]
async fn leaving_a_group_is_blocked_by_the_production_claim() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());
    let original = seed_original(&repo).await;
    let msg = seed_reference_for(&pool, 1, original.file_id).await;
    let group: (i64,) =
        sqlx::query_as("SELECT channel_id FROM privchat_messages WHERE message_id = $1")
            .bind(msg)
            .fetch_one(pool.as_ref())
            .await
            .expect("group id");

    const BARRIER: i64 = 973_002;
    let _barrier = InsertBarrier::arm(&pool, BARRIER).await;
    let mut holder = pool.begin().await.expect("holder tx");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(BARRIER)
        .execute(&mut *holder)
        .await
        .expect("hold barrier");

    let claim_pool = pool.clone();
    let source = original.clone();
    let claim = tokio::spawn(async move {
        FileUploadRepository::new(claim_pool)
            .copy_for_user(&source, OTHER as u64, "message", None)
            .await
    });
    let claim_pid = wait_for_claim_at_barrier(&pool, BARRIER).await;

    let (leave_pid, leaver) = spawn_blocked_write(
        pool.clone(),
        "UPDATE privchat_group_members SET left_at = now_millis()
          WHERE group_id = $1 AND user_id = $2",
        group.0,
        OTHER,
    )
    .await;
    wait_until_blocked_by(&pool, leave_pid, claim_pid).await;

    holder.commit().await.expect("release barrier");
    claim.await.expect("claim task").expect("claim succeeds");
    leaver.await.expect("leave task");

    cleanup(&pool).await;
}

/// 锁等待超时必须是**可重试**的，不是终局失败。
///
/// 占住 file_path 那把 advisory 锁不放，真实 claim 会在 `lock_timeout` 到点后
/// 拿到 `55P03`。包成 internal 的话，一次瞬时竞争就让这条附件永远发不出去。
#[tokio::test]
async fn a_lock_timeout_is_retryable_not_terminal() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());
    let original = seed_original(&repo).await;
    authorize_claimer(&pool, original.file_id, original.uploader_id as i64, OTHER).await;

    // 别人占着同一把 file_path 锁不放。
    let mut squatter = pool.begin().await.expect("squatter tx");
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(&original.file_path)
        .execute(&mut *squatter)
        .await
        .expect("squat the file lock");

    let result = repo
        .copy_for_user(&original, OTHER as u64, "message", None)
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
async fn wait_until_blocked_on_advisory_lock(pool: &sqlx::PgPool, key: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks \
             WHERE locktype = 'advisory' AND NOT granted \
               AND objid = (hashtext($1)::bigint & 4294967295)",
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

/// 摘要不是授权：拿得到 `stored_sha256` 不等于现在还能读这份文件。
///
/// 🔴 摘要会比访问权限活得久——读者退群、消息被删之后，那串哈希还在他手里。
/// 只认摘要就等于让任何拿到过哈希的人随时把文件领走。
///
/// 拒绝时必须与「服务端没有这份内容」返回同一句话，否则这个接口就成了文件
/// 存在性探测器：拿一堆摘要来问，能区分「没有」和「有但你无权」。
#[tokio::test]
async fn a_digest_alone_does_not_authorize_a_claim() {
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
                sha256: Some(SHA.to_string()),
                declared_size: Some(1024),
                mime_type: Some("image/png".to_string()),
                transform_version: 0,
            },
            UploadTokenPurpose::ClaimExisting,
        )
        .await
        .expect("token");

    let (messages, channels) = authorization_deps(&pool);
    let refused = claim_existing_file(
        &file_service,
        &token_service,
        &messages,
        &channels,
        OTHER as u64,
        &token.token,
        SHA,
    )
    .await;

    match refused {
        Err(e) => {
            let text = e.to_string();
            assert!(
                text.contains("没有这份内容"),
                "拒绝语必须与「内容不存在」一致，实际: {text}"
            );
        }
        Ok(meta) => panic!("🔴 无权者仅凭摘要就领到了 file_id={}", meta.file_id),
    }

    // 而且不能留下任何痕迹：被拒的 claim 不该写库。
    let claimed = repo
        .find_claimed(OTHER as u64, &privchat::service::file_claim_service::claim_key_hash(&token.token))
        .await
        .expect("query claimed");
    assert!(claimed.is_none(), "被拒的 claim 不该留下记录");

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
            .copy_for_user(&original, DIRECT_PEER as u64, "message", None)
            .await;
        assert!(
            claimed.is_ok(),
            "channel_id={channel} 的合法成员被误拒了：{claimed:?}"
        );
        cleanup(&pool).await;
    }
}

/// 授权检查通过之后、写入之前撤回消息 → 不能再开出新的 file_id。
///
/// 🔴 claim 是一次**新的授权动作**。「撤回收不回已经下载的东西」不等于「撤回之后
/// 还能继续开新的 file_id」——后者是在失权之后继续授予。
///
/// 这条不靠时序：测试自己按顺序走完「判定 → 撤回 → 写入」，正是那个窗口里
/// 会发生的事，所以确定性复现。
#[tokio::test]
async fn revoking_between_the_check_and_the_write_stops_the_claim() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    cleanup(&pool).await;
    let repo = FileUploadRepository::new(pool.clone());
    let original = seed_original(&repo).await;
    authorize_claimer(&pool, original.file_id, original.uploader_id as i64, OTHER).await;

    // 1) 判定：此刻是有权的（规范判据）。
    let (messages, channels) = authorization_deps(&pool);
    let decision = privchat::service::attachment_authorization::resolve_attachment_access(
        &messages,
        &channels,
        &original,
        OTHER as u64,
    )
    .await
    .expect("authorization available");
    assert!(decision.authorized, "前提：撤回之前是有权的");

    // 2) 撤回：判定与写入之间发生的事。
    sqlx::query("UPDATE privchat_messages SET revoked = true WHERE message_id = 970002")
        .execute(pool.as_ref())
        .await
        .expect("revoke");

    // 3) 写入：必须被事务内那道闸拦下。
    let refused = repo
        .copy_for_user(&original, OTHER as u64, "message", Some("race-key"))
        .await;
    assert!(
        refused.is_err(),
        "🔴 撤回之后不能再开出新的 file_id——那是在失权之后继续授予"
    );

    // 而且整事务回滚，不留半条记录。
    let rows: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM privchat_file_uploads WHERE uploader_id = $1 AND file_path = $2",
    )
    .bind(OTHER)
    .bind(&original.file_path)
    .fetch_one(pool.as_ref())
    .await
    .expect("count");
    assert_eq!(rows.0, 0, "被拒的 claim 不该留下任何行");

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
    let original = seed_original(&repo).await;
    authorize_claimer(&pool, original.file_id, original.uploader_id as i64, OTHER).await;

    let file_service = Arc::new(FileService::new(Vec::new(), 0, pool.clone()));
    let token_service = Arc::new(UploadTokenService::new());

    let identity = || UploadIdentity {
        sha256: Some(SHA.to_string()),
        declared_size: Some(1024),
        mime_type: Some("image/png".to_string()),
        transform_version: 0,
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
    authorize_claimer(&pool, original.file_id, original.uploader_id as i64, OTHER).await;
    let (messages, channels) = authorization_deps(&pool);
    let refused = claim_existing_file(
        &file_service,
        &token_service,
        &messages,
        &channels,
        OTHER as u64,
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
    assert!(claim_existing_file(
        &file_service,
        &token_service,
        &messages,
        &channels,
        OTHER as u64,
        &claim_token.token,
        SHA,
    )
    .await
    .is_ok());

    cleanup(&pool).await;
}
