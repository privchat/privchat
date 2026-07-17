//! CODEX-11 cross-node session ownership and dispatch bus.

use crate::infra::redis::RedisClient;
use crate::infra::{ConnectionManager, DeliveryReport};
use anyhow::{anyhow, Result};
use privchat_protocol::protocol::PushMessageRequest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

const NODE_LEASE_TTL_SECS: usize = 30;
const OWNER_LEASE_TTL_MS: i64 = 30_000;
const MAINTENANCE_INTERVAL_SECS: u64 = 10;
const DISPATCH_RESPONSE_TTL_SECS: usize = 30;
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SessionOwnerLease {
    node_id: String,
    device_id: String,
    session_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CrossNodeDispatchRequest {
    request_id: String,
    origin_node_id: String,
    target_node_id: String,
    user_id: u64,
    message: PushMessageRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CrossNodeDispatchResponse {
    request_id: String,
    attempted: usize,
    successful_session_ids: Vec<u64>,
    acknowledged_session_ids: Vec<u64>,
    failed_count: usize,
    error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RemoteDispatchOutcome {
    pub owner_node_id: String,
    pub attempted: usize,
    pub successful_session_ids: Vec<u64>,
    pub acknowledged_session_ids: Vec<u64>,
    pub failed_count: usize,
    pub error: Option<String>,
}

pub struct SessionOwnershipRegistry {
    redis: Arc<RedisClient>,
    node_id: String,
    node_lease_token: String,
}

impl SessionOwnershipRegistry {
    pub async fn claim(redis: Arc<RedisClient>, node_id: String) -> Result<Arc<Self>> {
        if node_id.trim().is_empty() || node_id == "local" {
            return Err(anyhow!(
                "PRIVCHAT_NODE_ID must be explicit and non-local in cluster mode"
            ));
        }
        let node_lease_token = uuid::Uuid::new_v4().to_string();
        let key = node_lease_key(&node_id);
        if !redis
            .set_nx_ex(&key, NODE_LEASE_TTL_SECS, &node_lease_token)
            .await?
        {
            return Err(anyhow!("cluster node id {node_id:?} is already leased"));
        }
        info!(node_id, "cross-node identity lease acquired");
        Ok(Arc::new(Self {
            redis,
            node_id,
            node_lease_token,
        }))
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub async fn register(
        &self,
        user_id: u64,
        device_id: &str,
        session_id: msgtrans::SessionId,
    ) -> Result<()> {
        let member = serde_json::to_string(&SessionOwnerLease {
            node_id: self.node_id.clone(),
            device_id: device_id.to_string(),
            session_id: session_id.as_u64(),
        })?;
        self.redis
            .zadd(
                &owner_key(user_id),
                (chrono::Utc::now().timestamp_millis() + OWNER_LEASE_TTL_MS) as f64,
                &member,
            )
            .await?;
        Ok(())
    }

    pub async fn unregister(
        &self,
        user_id: u64,
        device_id: &str,
        session_id: msgtrans::SessionId,
    ) -> Result<()> {
        let member = serde_json::to_string(&SessionOwnerLease {
            node_id: self.node_id.clone(),
            device_id: device_id.to_string(),
            session_id: session_id.as_u64(),
        })?;
        self.redis.zrem(&owner_key(user_id), &member).await?;
        Ok(())
    }

    pub async fn owner_nodes(&self, user_id: u64) -> Result<Vec<String>> {
        let key = owner_key(user_id);
        let now = chrono::Utc::now().timestamp_millis() as f64;
        self.redis
            .zremrangebyscore(&key, f64::NEG_INFINITY, now)
            .await?;
        let members = self
            .redis
            .zrangebyscore(&key, now, f64::INFINITY, None)
            .await?;
        let mut nodes = BTreeSet::new();
        for member in members {
            match serde_json::from_str::<SessionOwnerLease>(&member) {
                Ok(owner) => {
                    nodes.insert(owner.node_id);
                }
                Err(error) => warn!(%error, "ignoring malformed session owner lease"),
            }
        }
        Ok(nodes.into_iter().collect())
    }

    pub fn start_maintenance(self: &Arc<Self>, connections: Arc<ConnectionManager>) {
        let registry = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(MAINTENANCE_INTERVAL_SECS));
            loop {
                tick.tick().await;
                let lease_ok = registry
                    .redis
                    .compare_and_expire(
                        &node_lease_key(&registry.node_id),
                        &registry.node_lease_token,
                        NODE_LEASE_TTL_SECS,
                    )
                    .await
                    .unwrap_or(false);
                if !lease_ok {
                    error!(
                        node_id = registry.node_id,
                        "cross-node identity lease lost; terminating to prevent split brain"
                    );
                    std::process::exit(1);
                }
                for connection in connections.get_all_connections().await {
                    if let Err(error) = registry
                        .register(
                            connection.user_id,
                            &connection.device_id,
                            connection.session_id,
                        )
                        .await
                    {
                        warn!(%error, "refresh session owner lease failed");
                    }
                }
            }
        });
    }
}

pub struct CrossNodeDispatchBus {
    redis: Arc<RedisClient>,
    registry: Arc<SessionOwnershipRegistry>,
    connections: Arc<ConnectionManager>,
}

impl CrossNodeDispatchBus {
    pub fn new(
        redis: Arc<RedisClient>,
        registry: Arc<SessionOwnershipRegistry>,
        connections: Arc<ConnectionManager>,
    ) -> Arc<Self> {
        Arc::new(Self {
            redis,
            registry,
            connections,
        })
    }

    pub fn start_worker(self: &Arc<Self>) {
        let bus = Arc::clone(self);
        tokio::spawn(async move {
            let queue = dispatch_queue_key(bus.registry.node_id());
            loop {
                let raw = match bus.redis.lpop(&queue).await {
                    Ok(Some(raw)) => raw,
                    Ok(None) => {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                        continue;
                    }
                    Err(error) => {
                        warn!(%error, "cross-node dispatch queue read failed");
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                };
                let request = match serde_json::from_str::<CrossNodeDispatchRequest>(&raw) {
                    Ok(request) if request.target_node_id == bus.registry.node_id() => request,
                    Ok(_) => {
                        warn!("cross-node request reached the wrong node queue");
                        continue;
                    }
                    Err(error) => {
                        warn!(%error, "malformed cross-node dispatch request");
                        continue;
                    }
                };
                let response = match bus
                    .connections
                    .send_push_to_user(request.user_id, &request.message)
                    .await
                {
                    Ok(report) => response_from_report(&request.request_id, report),
                    Err(error) => CrossNodeDispatchResponse {
                        request_id: request.request_id.clone(),
                        attempted: 0,
                        successful_session_ids: Vec::new(),
                        acknowledged_session_ids: Vec::new(),
                        failed_count: 1,
                        error: Some(error.to_string()),
                    },
                };
                match serde_json::to_string(&response) {
                    Ok(encoded) => {
                        if let Err(error) = bus
                            .redis
                            .setex(
                                &dispatch_response_key(
                                    &request.origin_node_id,
                                    &request.request_id,
                                ),
                                DISPATCH_RESPONSE_TTL_SECS,
                                &encoded,
                            )
                            .await
                        {
                            warn!(%error, "cross-node dispatch response write failed");
                        }
                    }
                    Err(error) => warn!(%error, "cross-node dispatch response encode failed"),
                }
            }
        });
    }

    pub async fn dispatch_remote_owners(
        &self,
        user_id: u64,
        message: &PushMessageRequest,
    ) -> Result<Vec<RemoteDispatchOutcome>> {
        let remote_nodes: Vec<String> = self
            .registry
            .owner_nodes(user_id)
            .await?
            .into_iter()
            .filter(|node| node != self.registry.node_id())
            .collect();
        let futures = remote_nodes.into_iter().map(|node| {
            let message = message.clone();
            async move { self.dispatch_one(node, user_id, message).await }
        });
        Ok(futures::future::join_all(futures).await)
    }

    async fn dispatch_one(
        &self,
        owner_node_id: String,
        user_id: u64,
        message: PushMessageRequest,
    ) -> RemoteDispatchOutcome {
        let request_id = uuid::Uuid::new_v4().to_string();
        let request = CrossNodeDispatchRequest {
            request_id: request_id.clone(),
            origin_node_id: self.registry.node_id().to_string(),
            target_node_id: owner_node_id.clone(),
            user_id,
            message,
        };
        let encoded = match serde_json::to_string(&request) {
            Ok(encoded) => encoded,
            Err(error) => {
                return RemoteDispatchOutcome {
                    owner_node_id,
                    failed_count: 1,
                    error: Some(error.to_string()),
                    ..RemoteDispatchOutcome::default()
                };
            }
        };
        if let Err(error) = self
            .redis
            .rpush(&dispatch_queue_key(&owner_node_id), &encoded)
            .await
        {
            return RemoteDispatchOutcome {
                owner_node_id,
                failed_count: 1,
                error: Some(error.to_string()),
                ..RemoteDispatchOutcome::default()
            };
        }

        let response_key = dispatch_response_key(self.registry.node_id(), &request_id);
        let started = tokio::time::Instant::now();
        while started.elapsed() < DISPATCH_TIMEOUT {
            match self.redis.get(&response_key).await {
                Ok(Some(raw)) => {
                    let _ = self.redis.del(&response_key).await;
                    return match serde_json::from_str::<CrossNodeDispatchResponse>(&raw) {
                        Ok(response) => RemoteDispatchOutcome {
                            owner_node_id,
                            attempted: response.attempted,
                            successful_session_ids: response.successful_session_ids,
                            acknowledged_session_ids: response.acknowledged_session_ids,
                            failed_count: response.failed_count,
                            error: response.error,
                        },
                        Err(error) => RemoteDispatchOutcome {
                            owner_node_id,
                            failed_count: 1,
                            error: Some(error.to_string()),
                            ..RemoteDispatchOutcome::default()
                        },
                    };
                }
                Ok(None) => tokio::time::sleep(Duration::from_millis(25)).await,
                Err(error) => {
                    return RemoteDispatchOutcome {
                        owner_node_id,
                        failed_count: 1,
                        error: Some(error.to_string()),
                        ..RemoteDispatchOutcome::default()
                    };
                }
            }
        }
        RemoteDispatchOutcome {
            owner_node_id,
            failed_count: 1,
            error: Some("cross-node dispatch response timeout".to_string()),
            ..RemoteDispatchOutcome::default()
        }
    }
}

fn response_from_report(request_id: &str, report: DeliveryReport) -> CrossNodeDispatchResponse {
    let failed_count = report.failed_count();
    CrossNodeDispatchResponse {
        request_id: request_id.to_string(),
        attempted: report.attempted,
        successful_session_ids: report
            .successful_sessions
            .into_iter()
            .map(|session| session.as_u64())
            .collect(),
        acknowledged_session_ids: report
            .acknowledged_sessions
            .into_iter()
            .map(|session| session.as_u64())
            .collect(),
        failed_count,
        error: None,
    }
}

fn node_lease_key(node_id: &str) -> String {
    format!("privchat:cluster:node:{node_id}")
}

fn owner_key(user_id: u64) -> String {
    format!("privchat:cluster:owners:{user_id}")
}

fn dispatch_queue_key(node_id: &str) -> String {
    format!("privchat:cluster:dispatch:{node_id}")
}

fn dispatch_response_key(origin_node_id: &str, request_id: &str) -> String {
    format!("privchat:cluster:dispatch-response:{origin_node_id}:{request_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_keys_are_node_scoped() {
        assert_ne!(dispatch_queue_key("node-a"), dispatch_queue_key("node-b"));
        assert!(dispatch_response_key("node-a", "req-1").contains("node-a:req-1"));
    }

    #[test]
    fn typed_dispatch_roundtrip_preserves_large_ids() {
        let request = CrossNodeDispatchRequest {
            request_id: "request".to_string(),
            origin_node_id: "node-a".to_string(),
            target_node_id: "node-b".to_string(),
            user_id: u64::MAX - 1,
            message: PushMessageRequest {
                server_message_id: u64::MAX - 2,
                ..Default::default()
            },
        };
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: CrossNodeDispatchRequest = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.user_id, request.user_id);
        assert_eq!(
            decoded.message.server_message_id,
            request.message.server_message_id
        );
    }

    #[tokio::test]
    #[ignore = "requires PRIVCHAT_TEST_REDIS_URL"]
    async fn redis_leases_fail_closed_and_discover_remote_owner() {
        let url = std::env::var("PRIVCHAT_TEST_REDIS_URL")
            .expect("PRIVCHAT_TEST_REDIS_URL is required for this test");
        let redis = Arc::new(
            RedisClient::new(&crate::config::RedisConfig {
                url,
                pool_size: 4,
                min_idle: 1,
                connection_timeout_secs: 2,
                command_timeout_ms: 2_000,
                idle_timeout_secs: 30,
            })
            .await
            .unwrap(),
        );
        let suffix = uuid::Uuid::new_v4().to_string();
        let node_a = format!("test-a-{suffix}");
        let node_b = format!("test-b-{suffix}");
        let user_id = u64::MAX - 42;
        let registry_a = SessionOwnershipRegistry::claim(redis.clone(), node_a.clone())
            .await
            .unwrap();
        assert!(
            SessionOwnershipRegistry::claim(redis.clone(), node_a.clone())
                .await
                .is_err()
        );
        let registry_b = SessionOwnershipRegistry::claim(redis.clone(), node_b.clone())
            .await
            .unwrap();
        registry_b
            .register(user_id, "device-b", msgtrans::SessionId::from(77_u64))
            .await
            .unwrap();
        assert_eq!(
            registry_a.owner_nodes(user_id).await.unwrap(),
            vec![node_b.clone()]
        );

        redis.del(&owner_key(user_id)).await.unwrap();
        redis.del(&node_lease_key(&node_a)).await.unwrap();
        redis.del(&node_lease_key(&node_b)).await.unwrap();
    }
}
