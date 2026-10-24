//! Trusted provisioning of an owner/private-assistant Direct. No second session model.
use crate::error::ServerError;
use sqlx::PgPool;

pub const SOURCE: &str = "personal_assistant";

pub async fn guard_relationship_removal(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, a: u64, b: u64,
) -> Result<(), ServerError> {
    let db = |e| ServerError::Database(format!("assistant relationship guard: {e}"));
    let assistants: Vec<i64> = sqlx::query_scalar(
        "SELECT user_id FROM privchat_users WHERE user_id IN ($1,$2) AND user_type=3 ORDER BY user_id")
        .bind(a as i64).bind(b as i64).fetch_all(&mut **tx).await.map_err(db)?;
    for assistant in assistants {
        sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(assistant)
            .execute(&mut **tx).await.map_err(db)?;
    }
    // Ownership survives account disablement; do not use the active-only owner_channel lookup.
    let permanent: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM privchat_channels WHERE channel_type=0 AND create_source=$3
         AND ((direct_user1_id=$1 AND direct_user2_id=$2) OR (direct_user1_id=$2 AND direct_user2_id=$1)))")
        .bind(a as i64).bind(b as i64).bind(SOURCE).fetch_one(&mut **tx).await.map_err(db)?;
    if permanent {
        return Err(ServerError::Authorization("私人助理与主人的好友关系不可删除或拉黑".into()));
    }
    Ok(())
}

pub async fn owner_channel(
    pool: &PgPool,
    owner: u64,
    assistant: u64,
) -> Result<Option<u64>, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT c.channel_id FROM privchat_channels c
         JOIN privchat_users o ON o.user_id=$1 AND o.user_type=0 AND o.status=0
         JOIN privchat_users a ON a.user_id=$2 AND a.user_type=3 AND a.status=0
         WHERE c.channel_type=0 AND c.create_source=$3 AND c.create_source_id=($1::bigint)::text
         AND ((c.direct_user1_id=$1 AND c.direct_user2_id=$2) OR
              (c.direct_user1_id=$2 AND c.direct_user2_id=$1))",
    )
    .bind(owner as i64)
    .bind(assistant as i64)
    .bind(SOURCE)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0 as u64))
}

pub async fn authorized_pair(
    pool: &PgPool,
    channel: u64,
    a: u64,
    b: u64,
) -> Result<bool, sqlx::Error> {
    Ok(owner_channel(pool, a, b).await? == Some(channel)
        || owner_channel(pool, b, a).await? == Some(channel))
}

pub async fn owner_can_read_detail(
    pool: &PgPool, owner: u64, assistant: u64, source: &str, source_id: &str,
) -> Result<bool, sqlx::Error> {
    let Some(channel) = owner_channel(pool, owner, assistant).await? else { return Ok(false) };
    if source == "conversation" {
        return Ok(source_id.parse::<u64>().ok() == Some(channel));
    }
    if source != "friend" || source_id.parse::<u64>().ok() != Some(assistant) {
        return Ok(false);
    }
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM privchat_friendships
        WHERE user_id=$1 AND friend_id=$2 AND status=1)")
        .bind(owner as i64).bind(assistant as i64).fetch_one(pool).await
}

pub async fn ensure(pool: &PgPool, owner: u64, assistant: u64) -> Result<(u64, bool), ServerError> {
    if owner == 0
        || assistant == 0
        || owner > i64::MAX as u64
        || assistant > i64::MAX as u64
        || owner == assistant
    {
        return Err(ServerError::Validation(
            "invalid owner/assistant IDs".into(),
        ));
    }
    let db = |e| ServerError::Database(format!("assistant Direct: {e}"));
    let mut tx = pool.begin().await.map_err(db)?;
    // Serialize all provisions of this assistant, not just the requested pair.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(assistant as i64)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    let users: Vec<(i64, i16, i16)> = sqlx::query_as(
        "SELECT user_id,user_type,status FROM privchat_users WHERE user_id IN ($1,$2) ORDER BY user_id FOR UPDATE")
        .bind(owner as i64).bind(assistant as i64).fetch_all(&mut *tx).await.map_err(db)?;
    if !users.contains(&(owner as i64, 0, 0)) || !users.contains(&(assistant as i64, 3, 0)) {
        return Err(ServerError::Authorization(
            "active human owner and type3 assistant required".into(),
        ));
    }
    let existing: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT c.channel_id,c.direct_user1_id,c.direct_user2_id FROM privchat_channels c
         WHERE c.channel_type=0 AND (c.direct_user1_id=$1 OR c.direct_user2_id=$1)
         AND NOT EXISTS (
             SELECT 1 FROM privchat_users system_peer
             WHERE system_peer.user_type=1 AND c.create_source='system_login_notice'
             AND system_peer.user_id=CASE WHEN c.direct_user1_id=$1
                 THEN c.direct_user2_id ELSE c.direct_user1_id END
         ) FOR UPDATE OF c",
    )
    .bind(assistant as i64)
    .fetch_all(&mut *tx)
    .await
    .map_err(db)?;
    if existing
        .iter()
        .any(|r| r.1 != owner as i64 && r.2 != owner as i64)
    {
        return Err(ServerError::Authorization(
            "assistant already has another Direct owner".into(),
        ));
    }
    let created = existing.is_empty();
    let id = if let Some(row) = existing.first() {
        sqlx::query(
            "UPDATE privchat_channels SET create_source=$2,create_source_id=$3 WHERE channel_id=$1
             AND (create_source IS DISTINCT FROM $2 OR create_source_id IS DISTINCT FROM $3)",
        )
        .bind(row.0)
        .bind(SOURCE)
        .bind(owner.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        row.0
    } else {
        sqlx::query_scalar::<_,i64>("INSERT INTO privchat_channels
            (channel_type,direct_user1_id,direct_user2_id,create_source,create_source_id,created_at,updated_at)
            VALUES (0,$1,$2,$3,$4,$5,$5) RETURNING channel_id")
            .bind(owner.min(assistant) as i64).bind(owner.max(assistant) as i64)
            .bind(SOURCE).bind(owner.to_string()).bind(chrono::Utc::now().timestamp_millis())
            .fetch_one(&mut *tx).await.map_err(db)?
    };
    for (user, friend) in [(owner, assistant), (assistant, owner)] {
        sqlx::query("INSERT INTO privchat_friendships
            (user_id,friend_id,status,source,source_id,created_at,updated_at)
            VALUES ($1,$2,1,$3,$4,$5,$5)
            ON CONFLICT (user_id,friend_id) DO UPDATE SET
                status=1,source=EXCLUDED.source,source_id=EXCLUDED.source_id,updated_at=EXCLUDED.updated_at
            WHERE privchat_friendships.status<>1 OR privchat_friendships.source IS DISTINCT FROM EXCLUDED.source
                OR privchat_friendships.source_id IS DISTINCT FROM EXCLUDED.source_id")
            .bind(user as i64).bind(friend as i64).bind(SOURCE).bind(owner.to_string())
            .bind(chrono::Utc::now().timestamp_millis()).execute(&mut *tx).await.map_err(db)?;
    }
    // Legacy blocks cannot be reintroduced through the public service. Repair only this verified pair.
    sqlx::query("DELETE FROM privchat_blacklist WHERE (user_id=$1 AND blocked_user_id=$2)
        OR (user_id=$2 AND blocked_user_id=$1)")
        .bind(owner as i64).bind(assistant as i64).execute(&mut *tx).await.map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok((id as u64, created))
}

pub async fn received_event(
    pool: &PgPool,
    channel_id: u64,
    server_message_id: u64,
    from_user_id: u64,
    assistant_uid: u64,
    now: i64,
) -> Result<Option<crate::server_event::ServerEvent>, anyhow::Error> {
    if owner_channel(pool, from_user_id, assistant_uid).await? != Some(channel_id) {
        return Ok(None);
    }
    Ok(Some(crate::server_event::ServerEvent::new(
        "assistant.message_received",
        &serde_json::json!({
            "channel_id":channel_id,"server_message_id":server_message_id,
            "from_user_id":from_user_id,"assistant_uid":assistant_uid
        }),
        now,
    )?))
}
