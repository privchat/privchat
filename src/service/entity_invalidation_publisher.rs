use crate::infra::connection_manager::ConnectionManager;
use crate::Result;
use futures::stream::{self, StreamExt};
use privchat_protocol::protocol::{MessageSetting, PushMessageRequest};
use privchat_protocol::{
    encode_message, ContentMessageType, EntityInvalidation, EntityInvalidationBatch,
    EntityMutationHint, ENTITY_INVALIDATION_PUSH_TOPIC_V1,
};
use std::collections::BTreeSet;
use std::sync::Arc;

#[derive(Clone)]
pub struct EntityInvalidationPublisher {
    connection_manager: Arc<ConnectionManager>,
}

impl EntityInvalidationPublisher {
    pub fn new(connection_manager: Arc<ConnectionManager>) -> Self {
        Self { connection_manager }
    }

    pub async fn publish_to_user(
        &self,
        user_id: u64,
        items: Vec<EntityInvalidation>,
    ) -> Result<()> {
        self.publish_to_users([user_id], items).await
    }

    pub async fn publish_to_users(
        &self,
        user_ids: impl IntoIterator<Item = u64>,
        items: Vec<EntityInvalidation>,
    ) -> Result<()> {
        let recipients: BTreeSet<u64> = user_ids.into_iter().filter(|id| *id != 0).collect();
        if recipients.is_empty() || items.is_empty() {
            return Ok(());
        }
        let notification_id = crate::infra::snowflake::next_message_id();
        let committed_at_ms = chrono::Utc::now().timestamp_millis();
        let batch = EntityInvalidationBatch::new_v1(notification_id, items, committed_at_ms)
            .map_err(|error| crate::error::ServerError::Protocol(error.to_string()))?;
        let payload = encode_message(&batch)
            .map_err(|error| crate::error::ServerError::Protocol(error.to_string()))?;
        let push = PushMessageRequest {
            setting: MessageSetting::default(),
            msg_key: format!("entity_invalidation_{notification_id}"),
            server_message_id: notification_id,
            message_seq: 0,
            local_message_id: 0,
            stream_no: String::new(),
            stream_seq: 0,
            stream_flag: 0,
            timestamp: (committed_at_ms / 1_000).max(0) as u32,
            channel_id: 0,
            channel_type: 0,
            message_type: ContentMessageType::System.as_u32(),
            expire: 0,
            topic: ENTITY_INVALIDATION_PUSH_TOPIC_V1.to_string(),
            from_uid: 0,
            payload,
            deleted: false,
        };

        let connection_manager = self.connection_manager.clone();
        stream::iter(recipients)
            .for_each_concurrent(50, |user_id| {
                let connection_manager = connection_manager.clone();
                let push = push.clone();
                async move {
                    if let Err(error) = connection_manager.send_push_to_user(user_id, &push).await {
                        tracing::warn!(
                            user_id,
                            notification_id,
                            %error,
                            "entity invalidation online dispatch failed; entity sync remains authoritative"
                        );
                    }
                }
            })
            .await;
        Ok(())
    }

    pub async fn publish_friend_change(
        &self,
        user_ids: impl IntoIterator<Item = u64>,
        friend_id: u64,
        mutation_hint: EntityMutationHint,
    ) -> Result<()> {
        self.publish_to_users(
            user_ids,
            vec![EntityInvalidation {
                entity_type: "friend".to_string(),
                entity_id: Some(friend_id.to_string()),
                scope: None,
                // The sync cursor is authoritative. Zero tells clients to
                // pull from their local cursor when the mutation API does
                // not return the committed sync_version.
                target_version: 0,
                mutation_hint,
            }],
        )
        .await
    }

    pub async fn publish_friend_pair_change(
        &self,
        first_user_id: u64,
        second_user_id: u64,
        mutation_hint: EntityMutationHint,
    ) -> Result<()> {
        self.publish_friend_change([first_user_id], second_user_id, mutation_hint)
            .await?;
        self.publish_friend_change([second_user_id], first_user_id, mutation_hint)
            .await
    }

    pub async fn publish_group_projection_change(
        &self,
        user_ids: impl IntoIterator<Item = u64>,
        group_id: u64,
        mutation_hint: EntityMutationHint,
    ) -> Result<()> {
        self.publish_to_users(
            user_ids,
            vec![
                EntityInvalidation {
                    entity_type: "group".to_string(),
                    entity_id: Some(group_id.to_string()),
                    scope: None,
                    target_version: 0,
                    mutation_hint: EntityMutationHint::Upsert,
                },
                EntityInvalidation {
                    entity_type: "channel".to_string(),
                    entity_id: Some(group_id.to_string()),
                    scope: None,
                    target_version: 0,
                    mutation_hint: EntityMutationHint::Upsert,
                },
                EntityInvalidation {
                    entity_type: "group_member".to_string(),
                    entity_id: None,
                    scope: Some(group_id.to_string()),
                    target_version: 0,
                    mutation_hint,
                },
            ],
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use privchat_protocol::{decode_message, EntityMutationHint};

    #[test]
    fn publisher_payload_is_typed_control_plane_not_chat_json() {
        let batch = EntityInvalidationBatch::new_v1(
            9_007_199_254_740_993,
            vec![EntityInvalidation {
                entity_type: "friend".to_string(),
                entity_id: Some("100000001".to_string()),
                scope: None,
                target_version: 42,
                mutation_hint: EntityMutationHint::Upsert,
            }],
            1_780_000_000_123,
        )
        .unwrap();
        let bytes = encode_message(&batch).unwrap();
        let decoded: EntityInvalidationBatch = decode_message(&bytes).unwrap();
        assert_eq!(decoded, batch);
        assert_ne!(bytes.first(), Some(&b'{'));
    }
}
