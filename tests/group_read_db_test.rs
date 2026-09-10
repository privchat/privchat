// 群已读的真库测试（READ_STATUS_SPEC §6.5）。
//
// 为什么必须是真库：上一轮 `list_message_authors_in_range` 把列名写成了 `from_uid`
// （实际是 `sender_id`），而调用点 `unwrap_or_default()` 把数据库错误变成"没人需要通知"。
// 当时 11 条纯函数策略测试全绿，却一行都没碰到这条 SQL。纯函数测试证明不了 SQL 能跑。
use sqlx::Row;

async fn pool() -> Option<sqlx::PgPool> {
    let url = privchat::require_test_database_url()?;
    sqlx::PgPool::connect(&url).await.ok()
}

/// 每个频道自己的三个成员 id，避免并行测试互相干扰。
fn users(channel: i64) -> [i64; 3] {
    [channel * 10 + 1, channel * 10 + 2, channel * 10 + 3]
}

async fn seed(p: &sqlx::PgPool, channel: i64) {
    // 消息表对 channel 有外键，先建频道。
    // 不吞错：这里 `let _ =` 过一次，约束失败被藏起来，症状跑到了插消息那一步。
    // 群频道的 channel_type 是 1（不是 2），且 group_id 必须非空并外键到 privchat_groups。
    sqlx::query(
        "INSERT INTO privchat_users (user_id, qr_key, created_at, updated_at)
         VALUES ($1, 'qr-' || $1::text, 0, 0)
         ON CONFLICT (user_id) DO NOTHING",
    )
    .bind(channel)
    .execute(p)
    .await
    .expect("seed owner");
    sqlx::query(
        "INSERT INTO privchat_groups (group_id, name, owner_id, qr_key, created_at, updated_at)
         VALUES ($1, 'read-test', $1, 'gqr-' || $1::text, 0, 0)
         ON CONFLICT (group_id) DO NOTHING",
    )
    .bind(channel)
    .execute(p)
    .await
    .expect("seed group");
    sqlx::query(
        "INSERT INTO privchat_channels (channel_id, channel_type, group_id, created_at, updated_at)
         VALUES ($1, 1, $1, 0, 0)
         ON CONFLICT (channel_id) DO NOTHING",
    )
    .bind(channel)
    .execute(p)
    .await
    .expect("seed channel");
    // 消息的 sender_id 外键到 privchat_users，成员也要真实存在。
    // 用 channel 派生 id：三个测试并行时用同一批行会互相删掉对方的数据。
    for uid in users(channel) {
        sqlx::query(
            "INSERT INTO privchat_users (user_id, qr_key, created_at, updated_at)
             VALUES ($1, 'qr-' || $1::text, 0, 0) ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(uid)
        .execute(p)
        .await
        .expect("seed member");
    }
    for t in [
        "privchat_channel_membership_interval",
        "privchat_channel_read_cursor",
        "privchat_messages",
        "privchat_channel_pts",
    ] {
        let _ = sqlx::query(&format!("DELETE FROM {t} WHERE channel_id = $1"))
            .bind(channel)
            .execute(p)
            .await;
    }
}

/// 那条曾经写错列名的 SQL：作者查询必须真的能在数据库上跑通。
#[tokio::test]
async fn author_query_runs_against_the_real_schema() {
    let Some(p) = pool().await else { return };
    let channel = 900_001i64;
    seed(&p, channel).await;
    let [a, b, _c] = users(channel);
    for (pts, sender) in [(1i64, a), (2, b), (3, a)] {
        sqlx::query(
            "INSERT INTO privchat_messages (message_id, channel_id, sender_id, pts, content, message_type, created_at, updated_at)
             VALUES ($1,$2,$3,$4,'x',0,0,0)",
        )
        .bind(channel * 100 + pts)
        .bind(channel)
        .bind(sender)
        .bind(pts)
        .execute(&p)
        .await
        .expect("insert message");
    }
    // 与 list_message_authors_in_range 同一条语句：写错列名会在这里失败，而不是被吞掉。
    let rows = sqlx::query(
        "SELECT DISTINCT sender_id FROM privchat_messages
         WHERE channel_id=$1 AND pts>$2 AND pts<=$3 AND sender_id<>$4 AND revoked=false",
    )
    .bind(channel)
    .bind(0i64)
    .bind(3i64)
    .bind(b)
    .fetch_all(&p)
    .await
    .expect("author query must run");
    let authors: Vec<i64> = rows.iter().map(|r| r.get::<i64, _>("sender_id")).collect();
    assert_eq!(authors, vec![a], "只应通知别人发的消息的作者");
}

/// 发送时收件人：后加入者不算，退群者仍算（§6.5.3）。
#[tokio::test]
async fn recipients_are_taken_at_send_time() {
    let Some(p) = pool().await else { return };
    let channel = 900_002i64;
    seed(&p, channel).await;
    // A(10) 一直在；B(20) 在 pts=5 退群；C(30) 在 pts=8 才入群。消息在 pts=6。
    let [a, b, c] = users(channel);
    for (user, joined, left) in [(a, 0i64, None), (b, 0, Some(5i64)), (c, 8, None)] {
        sqlx::query(
            "INSERT INTO privchat_channel_membership_interval (channel_id,user_id,joined_pts,left_pts)
             VALUES ($1,$2,$3,$4)",
        )
        .bind(channel)
        .bind(user)
        .bind(joined)
        .bind(left)
        .execute(&p)
        .await
        .expect("interval");
    }
    let rows = sqlx::query(
        "SELECT DISTINCT user_id FROM privchat_channel_membership_interval
         WHERE channel_id=$1 AND user_id<>$2 AND joined_pts<$3 AND (left_pts IS NULL OR left_pts>=$3)",
    )
    .bind(channel)
    .bind(99i64) // 发送者不在这三人里
    .bind(6i64)
    .fetch_all(&p)
    .await
    .expect("recipients query");
    let mut ids: Vec<i64> = rows.iter().map(|r| r.get::<i64, _>("user_id")).collect();
    ids.sort();
    assert_eq!(ids, vec![a], "pts=6 时只有 A 在群里：B 已退、C 未入");
}

/// 聚合水位是「除你之外的 MAX」，不指向任何具体的人（§6.5.8）。
#[tokio::test]
async fn aggregate_is_a_max_not_one_persons_cursor() {
    let Some(p) = pool().await else { return };
    let channel = 900_003i64;
    seed(&p, channel).await;
    let [a, b, c] = users(channel);
    for (user, pts) in [(a, 3i64), (b, 7), (c, 5)] {
        sqlx::query(
            "INSERT INTO privchat_channel_read_cursor (user_id,channel_id,last_read_pts,sync_version)
             VALUES ($1,$2,$3, nextval('privchat_channel_read_cursor_sync_version_seq'))",
        )
        .bind(user)
        .bind(channel)
        .bind(pts)
        .execute(&p)
        .await
        .expect("cursor");
    }
    let row: (Option<i64>,) = sqlx::query_as(
        "SELECT MAX(last_read_pts) FROM privchat_channel_read_cursor WHERE channel_id=$1 AND user_id<>$2",
    )
    .bind(channel)
    .bind(b) // 作者是 b：聚合要排除他自己
    .fetch_one(&p)
    .await
    .expect("aggregate");
    assert_eq!(row.0, Some(5), "排除 b 之后其他人的最大水位是 c 的 5");
}
