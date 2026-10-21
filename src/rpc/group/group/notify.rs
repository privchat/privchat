use crate::rpc::RpcServiceContext;

/// Best-effort acceleration over the existing channel subscription transport.
/// Consumers must also refresh authoritative RPC projections on resume.
pub async fn publish_changed(services: &RpcServiceContext, channel_id: u64, topic: &str) {
    let request = privchat_protocol::protocol::PublishRequest {
        channel_id,
        topic: Some(topic.to_owned()),
        timestamp: chrono::Utc::now().timestamp().max(0) as u64,
        payload: b"{}".to_vec(),
        publisher: None,
        server_message_id: None,
    };
    let bytes = match privchat_protocol::encode_message(&request) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "cannot encode projection invalidation");
            return;
        }
    };
    let sessions = services.subscribe_manager.get_channel_sessions(channel_id);
    let transport = services.connection_manager.transport_server.read().await;
    if let Some(server) = transport.as_ref() {
        for session in sessions {
            let options = msgtrans::SendOptions::new()
                .biz_type(privchat_protocol::protocol::MessageType::PublishRequest as u8);
            if let Err(error) = server
                .send_with_options(session, bytes.clone().into(), options)
                .await
            {
                tracing::warn!(%error, channel_id, topic, "projection invalidation delivery failed");
            }
        }
    }
}
